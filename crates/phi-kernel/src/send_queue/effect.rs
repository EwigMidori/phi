//! Turn disposition + stream/terminal commit units; single [`EffectApplier`] commits.
//!
//! ## Stream vs terminal
//!
//! - **Stream** frames use [`EffectBatch`]: authority intents in `writes`, pure
//!   observations in `notices`. Committed by [`EffectApplier::apply_stream`].
//! - **Terminal** jobs use [`TurnOutcome`] only (not [`EffectBatch`]). Committed by
//!   [`EffectApplier::apply_terminal`].
//!
//! ## Stream commit order (hard)
//!
//! [`EffectBatch`] is **not** a timeline of side effects. [`EffectApplier::apply_stream`]:
//!
//! 1. For each [`TranscriptWrite`] in order: `record_*` on the transcript; **on success**,
//!    immediately `bus.send(project_write(...))` (tool observation projected from the write).
//! 2. Then for each `notice` in `batch.notices`: `bus.send(notice)`.
//!
//! Durable tool rows are the source of truth for tool facts on the stream path.
//! Bus events for tool call/result are projected after successful record — not dual-built
//! in the turn with duplicated fields.
//!
//! Text/start/approval/unknown stay notice-only. Reasoning is buffered then written
//! as a durable sibling row when the turn leaves the reasoning span (or at end).

use crate::agent::{ToolCallId, ToolName, ToolResultStatus};
use crate::error::Result;
use crate::events::{EventBus, KernelEvent};
use crate::ids::{JobId, MessageId, SessionId};
use crate::transcript::Transcript;

use super::job::GenerationJob;

/// Loop signal from [`super::turn::GenerationTurn::handle`] — not `Option` / `Ok(None)`.
#[derive(Debug)]
pub(crate) enum Disposition {
    Continue,
    Terminal(TurnTerminal),
}

/// How the **agent stream** signals end (from [`AgentEvent`](crate::agent::AgentEvent)).
///
/// Mapped into [`TurnOutcome`] by [`super::turn::GenerationTurn::drive`], after
/// cancel/stop is considered (abort wins over stream terminal).
#[derive(Debug)]
pub(crate) enum TurnTerminal {
    /// Clean agent finish ([`AgentEvent::Finished`](crate::agent::AgentEvent::Finished)).
    Finished,
    /// Agent-reported failure ([`AgentEvent::Error`](crate::agent::AgentEvent::Error)).
    Failed(String),
}

/// Terminal job outcome after [`super::turn::GenerationTurn::drive`] (or early exit
/// in [`super::queue::SendQueue::run_until_idle`]).
///
/// | Variant | Meaning | Typical causes | Bus event | `record_assistant`? |
/// |---------|---------|----------------|-----------|---------------------|
/// | [`Completed`](Self::Completed) | Normal success | stream finished cleanly | `GenerationDone` (or Stopped if write soft-skipped) | yes |
/// | [`Aborted`](Self::Aborted) | **External interrupt** — stop/cancel, not a fault | `stop`, cancel-epoch stale, pump abort before/during stream | `GenerationStopped` + **partial** buffer | **no** |
/// | [`Failed`](Self::Failed) | **Fault** — agent/stream/precondition error | `AgentEvent::Error`, stream `Err`, `agent.run` Err, empty dialogue | `GenerationError` + **message** | **no** |
///
/// **Aborted vs Failed:** Aborted = intentional/preempted stop (user or cancel fence),
/// payload is partial text. Failed = something went wrong, payload is an error string.
/// Both skip durable assistant content; only Completed tries to record the buffer.
#[derive(Debug)]
pub(crate) enum TurnOutcome {
    /// Successful generation; buffer is the assistant body to record.
    Completed {
        job: GenerationJob,
        assistant_message_id: MessageId,
        content: String,
    },
    /// Stopped or cancelled mid-flight. Does not mean the model crashed.
    Aborted {
        job: GenerationJob,
        assistant_message_id: MessageId,
        /// Text already buffered when interrupted (may be empty).
        partial: String,
    },
    /// Generation failed; `message` is for operators/UI error surfaces.
    Failed { job: GenerationJob, message: String },
}

impl TurnOutcome {
    #[must_use]
    pub(crate) fn job_id(&self) -> &JobId {
        match self {
            Self::Completed { job, .. } | Self::Aborted { job, .. } | Self::Failed { job, .. } => {
                &job.job_id
            }
        }
    }
}

/// Authority intent: durable transcript row from the stream path.
#[derive(Debug)]
pub(crate) enum TranscriptWrite {
    /// Sibling CoT row ([`crate::agent::TurnItem::Reasoning`]).
    Reasoning { content: String },
    ToolCall {
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        input: serde_json::Value,
    },
    ToolResult {
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        output: serde_json::Value,
        status: ToolResultStatus,
    },
}

/// Stream-frame intents for one handle/start/seal batch.
///
/// **Not** a timeline: applier always commits `writes` (each record then projected
/// notice) first, then `notices`.
#[derive(Debug, Default)]
pub(crate) struct EffectBatch {
    /// Authority intents (Transcript).
    pub writes: Vec<TranscriptWrite>,
    /// Observation only (EventBus) — pure notices, not projected from writes.
    pub notices: Vec<KernelEvent>,
}

impl EffectBatch {
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    #[allow(dead_code)] // batch query helper (tests + future call sites)
    pub(crate) fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.notices.is_empty()
    }

    pub(crate) fn push_write(&mut self, write: TranscriptWrite) {
        self.writes.push(write);
    }

    #[allow(dead_code)] // batch builder helper (tests + future call sites)
    pub(crate) fn push_notice(&mut self, notice: KernelEvent) {
        self.notices.push(notice);
    }

    #[must_use]
    pub(crate) fn from_notice(notice: KernelEvent) -> Self {
        Self {
            writes: Vec::new(),
            notices: vec![notice],
        }
    }

    #[must_use]
    pub(crate) fn from_write(write: TranscriptWrite) -> Self {
        Self {
            writes: vec![write],
            notices: Vec::new(),
        }
    }
}

/// Unified commit unit for the sole applier path.
#[derive(Debug)]
pub(crate) enum CommitUnit<'a> {
    /// Stream/start/seal batch for a claimed job.
    Stream {
        job_id: &'a JobId,
        batch: EffectBatch,
    },
    /// Terminal Done / Stopped / Error.
    Terminal(TurnOutcome),
}

/// Project a durable tool write into the matching generation bus event.
///
/// Reasoning writes are durable-only (live path already emitted
/// [`KernelEvent::GenerationReasoningDelta`] notices).
fn project_write(session_id: &SessionId, job_id: &JobId, write: &TranscriptWrite) -> Option<KernelEvent> {
    match write {
        TranscriptWrite::Reasoning { .. } => None,
        TranscriptWrite::ToolCall {
            tool_call_id,
            tool_name,
            input,
        } => Some(KernelEvent::GenerationToolCall {
            session_id: session_id.clone(),
            job_id: job_id.clone(),
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            input: input.clone(),
        }),
        TranscriptWrite::ToolResult {
            tool_call_id,
            tool_name: _,
            output,
            status,
        } => Some(KernelEvent::GenerationToolResult {
            session_id: session_id.clone(),
            job_id: job_id.clone(),
            tool_call_id: tool_call_id.clone(),
            output: output.clone(),
            status: *status,
        }),
    }
}

/// Sole commit path for generation: transcript authority then observation fan-out.
///
/// Holds ports for one session pump step; no generation `bus.send` / record outside this type.
///
/// # Stream vs terminal
///
/// - [`Self::apply_stream`]: each write → record then projected tool notice; then batch notices.
/// - [`Self::apply_terminal`]: record (or version read) before terminal emit.
/// - [`Self::commit`]: thin match over [`CommitUnit`].
pub(crate) struct EffectApplier<'a> {
    session_id: &'a SessionId,
    transcript: &'a dyn Transcript,
    bus: &'a EventBus,
}

impl<'a> EffectApplier<'a> {
    #[must_use]
    pub(crate) fn new(
        session_id: &'a SessionId,
        transcript: &'a dyn Transcript,
        bus: &'a EventBus,
    ) -> Self {
        Self {
            session_id,
            transcript,
            bus,
        }
    }

    /// Thin dispatcher: stream batch or terminal outcome.
    pub(crate) fn commit(&self, unit: CommitUnit<'_>) -> Result<()> {
        match unit {
            CommitUnit::Stream { job_id, batch } => self.apply_stream(job_id, batch),
            CommitUnit::Terminal(outcome) => self.apply_terminal(outcome),
        }
    }

    /// Commit stream/start/seal effects for `job_id`.
    ///
    /// For each write: `record_*` then immediately `bus.send(project_write(...))`.
    /// Then send each pure `notice` in order.
    pub(crate) fn apply_stream(&self, job_id: &JobId, batch: EffectBatch) -> Result<()> {
        for write in &batch.writes {
            match write {
                TranscriptWrite::Reasoning { content } => {
                    let _ = self.transcript.record_reasoning(self.session_id, content)?;
                }
                TranscriptWrite::ToolCall {
                    tool_call_id,
                    tool_name,
                    input,
                } => {
                    let _ = self.transcript.record_tool_call(
                        self.session_id,
                        tool_call_id,
                        tool_name,
                        input,
                    )?;
                }
                TranscriptWrite::ToolResult {
                    tool_call_id,
                    tool_name,
                    output,
                    status,
                } => {
                    let _ = self.transcript.record_tool_result(
                        self.session_id,
                        tool_call_id,
                        tool_name,
                        output,
                        *status,
                    )?;
                }
            }
            if let Some(ev) = project_write(self.session_id, job_id, write) {
                let _ = self.bus.send(ev);
            }
        }
        for notice in batch.notices {
            let _ = self.bus.send(notice);
        }
        Ok(())
    }

    /// Commit a terminal job outcome (assistant write + Done / Stopped / Error).
    ///
    /// Record (or version read) before emit — same authority-before-observation spirit
    /// as [`Self::apply_stream`]. Terminal jobs use [`TurnOutcome`], not [`EffectBatch`].
    ///
    /// - [`TurnOutcome::Completed`] → try `record_assistant` → `GenerationDone`
    ///   (if soft-skip write: still emit `GenerationStopped` with empty partial — not Aborted)
    /// - [`TurnOutcome::Aborted`] → no assistant write → `GenerationStopped` + partial
    /// - [`TurnOutcome::Failed`] → no assistant write → `GenerationError` + message
    pub(crate) fn apply_terminal(&self, outcome: TurnOutcome) -> Result<()> {
        match outcome {
            TurnOutcome::Completed {
                job,
                assistant_message_id,
                content,
            } => {
                let recorded = self.transcript.record_assistant(
                    self.session_id,
                    &assistant_message_id,
                    &content,
                )?;
                let event = if recorded.wrote {
                    KernelEvent::GenerationDone {
                        session_id: self.session_id.clone(),
                        job_id: job.job_id,
                        assistant_message_id,
                        content,
                        version: recorded.version,
                    }
                } else {
                    // Soft-skip (session not live): observation only — not TurnOutcome::Aborted.
                    KernelEvent::GenerationStopped {
                        session_id: self.session_id.clone(),
                        job_id: job.job_id,
                        assistant_message_id,
                        partial: String::new(),
                        version: recorded.version,
                    }
                };
                let _ = self.bus.send(event);
                Ok(())
            }
            // External interrupt: keep partial text for UI, do not durable-record as assistant.
            TurnOutcome::Aborted {
                job,
                assistant_message_id,
                partial,
            } => {
                let version = self.transcript.version()?;
                let _ = self.bus.send(KernelEvent::GenerationStopped {
                    session_id: self.session_id.clone(),
                    job_id: job.job_id,
                    assistant_message_id,
                    partial,
                    version,
                });
                Ok(())
            }
            // Fault path: surface message; no partial assistant identity required.
            TurnOutcome::Failed { job, message } => {
                let version = self.transcript.version()?;
                let _ = self.bus.send(KernelEvent::GenerationError {
                    session_id: self.session_id.clone(),
                    job_id: job.job_id,
                    message,
                    version,
                });
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MessageId;
    use crate::transcript::InMemoryTranscript;

    fn bus() -> EventBus {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        tx
    }

    #[test]
    fn apply_stream_tool_call_write_projects_generation_tool_call() {
        let session_id = SessionId::generate();
        let job_id = JobId::generate();
        let transcript = InMemoryTranscript::new();
        transcript.ensure_live(&session_id).unwrap();
        let bus = bus();
        let mut rx = bus.subscribe();
        let applier = EffectApplier::new(&session_id, &transcript, &bus);

        let batch = EffectBatch::from_write(TranscriptWrite::ToolCall {
            tool_call_id: ToolCallId::new("tc-proj"),
            tool_name: ToolName::new("echo"),
            input: serde_json::json!({"x": 1}),
        });
        applier.apply_stream(&job_id, batch).unwrap();

        let event = rx.try_recv().expect("projected GenerationToolCall");
        match event {
            KernelEvent::GenerationToolCall {
                session_id: sid,
                job_id: jid,
                tool_call_id,
                tool_name,
                input,
            } => {
                assert_eq!(sid, session_id);
                assert_eq!(jid, job_id);
                assert_eq!(tool_call_id.as_str(), "tc-proj");
                assert_eq!(tool_name.as_str(), "echo");
                assert_eq!(input, serde_json::json!({"x": 1}));
            }
            other => panic!("expected GenerationToolCall, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "no extra bus events");
    }

    #[test]
    fn apply_stream_notices_after_projected_writes() {
        let session_id = SessionId::generate();
        let job_id = JobId::generate();
        let transcript = InMemoryTranscript::new();
        transcript.ensure_live(&session_id).unwrap();
        let bus = bus();
        let mut rx = bus.subscribe();
        let applier = EffectApplier::new(&session_id, &transcript, &bus);

        let mut batch = EffectBatch::from_write(TranscriptWrite::ToolCall {
            tool_call_id: ToolCallId::new("tc1"),
            tool_name: ToolName::new("echo"),
            input: serde_json::json!({}),
        });
        batch.push_notice(KernelEvent::GenerationTextDelta {
            session_id: session_id.clone(),
            job_id: job_id.clone(),
            assistant_message_id: MessageId::generate(),
            text: "after".into(),
        });
        applier.apply_stream(&job_id, batch).unwrap();

        assert!(matches!(
            rx.try_recv().unwrap(),
            KernelEvent::GenerationToolCall { .. }
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            KernelEvent::GenerationTextDelta { text, .. } if text == "after"
        ));
    }
}
