//! One in-flight generation for a claimed [`GenerationJob`](super::GenerationJob).
//!
//! **SRP:** owns stream-event interpretation, text buffer, [`ToolLedger`], and
//! terminal classification. [`handle`](GenerationTurn::handle) is pure state +
//! effects; [`drive`](GenerationTurn::drive) owns the stream loop and commits via
//! [`EffectApplier`](super::effect::EffectApplier). [`SendQueue`](super::SendQueue)
//! only claims / pumps / finishes jobs.
//!
//! **Effects:** `handle` / `start_effects` / `seal_incomplete_effects` return
//! [`EffectBatch`](super::effect::EffectBatch) **intents**. Tool authority is
//! [`TranscriptWrite`](super::effect::TranscriptWrite) only; tool bus events are
//! projected by the applier after successful record. Non-tool frames are notices only.

use futures::StreamExt;

use crate::agent::{
    AgentEvent, AgentEventStream, ToolCallId, ToolCallSealPolicy, ToolName, ToolResultStatus,
    TurnCancel, TurnItem,
};
use crate::error::Result;
use crate::events::KernelEvent;
use crate::ids::{JobId, MessageId, SessionId};

use super::effect::{
    CommitUnit, Disposition, EffectApplier, EffectBatch, TranscriptWrite, TurnOutcome, TurnTerminal,
};
use super::epoch::Epoch;
use super::job::GenerationJob;
use super::tool_ledger::ToolLedger;

pub(crate) struct GenerationTurn {
    job: GenerationJob,
    assistant_message_id: MessageId,
    claimed_epoch: Epoch,
    history: Vec<TurnItem>,
    /// Answer body; recorded as assistant at terminal success.
    buffer: String,
    /// Open CoT span; flushed as [`TurnItem::Reasoning`] before text/tools/end.
    reasoning_buffer: String,
    tool_ledger: ToolLedger,
}

impl GenerationTurn {
    #[must_use]
    pub(crate) fn begin(
        job: GenerationJob,
        claimed_epoch: Epoch,
        history: Vec<TurnItem>,
    ) -> Self {
        Self {
            job,
            assistant_message_id: MessageId::generate(),
            claimed_epoch,
            history,
            buffer: String::new(),
            reasoning_buffer: String::new(),
            tool_ledger: ToolLedger::new(),
        }
    }

    #[must_use]
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.job.session_id
    }

    #[must_use]
    pub(crate) fn claimed_epoch(&self) -> Epoch {
        self.claimed_epoch
    }

    #[must_use]
    pub(crate) fn history(&self) -> &[TurnItem] {
        &self.history
    }

    #[must_use]
    pub(crate) fn job_id(&self) -> &JobId {
        &self.job.job_id
    }

    /// Mechanism effects for turn start — committed only via [`EffectApplier`].
    /// Notice-only (no transcript write).
    #[must_use]
    pub(crate) fn start_effects(&self) -> EffectBatch {
        EffectBatch::from_notice(KernelEvent::GenerationStart {
            session_id: self.session_id().clone(),
            job_id: self.job.job_id.clone(),
            user_message_id: self.job.user_message_id.clone(),
            assistant_message_id: self.assistant_message_id.clone(),
        })
    }

    /// Interpret one agent event: update buffers / ledger; return disposition + effects.
    ///
    /// Does **not** touch transcript or bus — [`EffectApplier`] commits.
    /// Tool success path: write only (projection in applier). Unknown tool result:
    /// notice only. Non-tool frames: notices only.
    pub(crate) fn handle(&mut self, event: AgentEvent) -> (Disposition, EffectBatch) {
        match event {
            AgentEvent::TextDelta { text } => self.on_text_delta(text),
            AgentEvent::ReasoningDelta { text } => self.on_reasoning_delta(text),
            AgentEvent::ToolCall {
                tool_call_id,
                tool_name,
                input,
            } => self.on_tool_call(tool_call_id, tool_name, input),
            AgentEvent::ToolResult {
                tool_call_id,
                output,
                status,
            } => self.on_tool_result(tool_call_id, output, status),
            AgentEvent::ToolApprovalRequired {
                tool_call_id,
                tool_name,
                input,
            } => (
                Disposition::Continue,
                EffectBatch::from_notice(KernelEvent::GenerationToolApprovalRequired {
                    session_id: self.session_id().clone(),
                    job_id: self.job.job_id.clone(),
                    tool_call_id,
                    tool_name,
                    input,
                }),
            ),
            AgentEvent::Unknown { kind, payload } => (
                Disposition::Continue,
                EffectBatch::from_notice(KernelEvent::GenerationAgentUnknown {
                    session_id: self.session_id().clone(),
                    job_id: self.job.job_id.clone(),
                    kind,
                    payload,
                }),
            ),
            AgentEvent::Error { message } => (
                Disposition::Terminal(TurnTerminal::Failed(message)),
                EffectBatch::empty(),
            ),
            AgentEvent::Finished { reason: _ } => (
                Disposition::Terminal(TurnTerminal::Finished),
                EffectBatch::empty(),
            ),
        }
    }

    /// Flush open CoT into a durable reasoning write (if any).
    fn take_reasoning_write(&mut self) -> Option<TranscriptWrite> {
        if self.reasoning_buffer.is_empty() {
            return None;
        }
        Some(TranscriptWrite::Reasoning {
            content: std::mem::take(&mut self.reasoning_buffer),
        })
    }

    fn prepend_reasoning_flush(&mut self, mut batch: EffectBatch) -> EffectBatch {
        if let Some(write) = self.take_reasoning_write() {
            let mut out = EffectBatch::from_write(write);
            out.writes.append(&mut batch.writes);
            out.notices.append(&mut batch.notices);
            out
        } else {
            batch
        }
    }

    fn on_text_delta(&mut self, text: String) -> (Disposition, EffectBatch) {
        if text.is_empty() {
            return (Disposition::Continue, EffectBatch::empty());
        }
        self.buffer.push_str(&text);
        let batch = EffectBatch::from_notice(KernelEvent::GenerationTextDelta {
            session_id: self.session_id().clone(),
            job_id: self.job.job_id.clone(),
            assistant_message_id: self.assistant_message_id.clone(),
            text,
        });
        (
            Disposition::Continue,
            self.prepend_reasoning_flush(batch),
        )
    }

    fn on_reasoning_delta(&mut self, text: String) -> (Disposition, EffectBatch) {
        if text.is_empty() {
            return (Disposition::Continue, EffectBatch::empty());
        }
        self.reasoning_buffer.push_str(&text);
        (
            Disposition::Continue,
            EffectBatch::from_notice(KernelEvent::GenerationReasoningDelta {
                session_id: self.session_id().clone(),
                job_id: self.job.job_id.clone(),
                assistant_message_id: self.assistant_message_id.clone(),
                text,
            }),
        )
    }

    /// Ledger open + durable write only; bus projection after successful record.
    fn on_tool_call(
        &mut self,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        input: serde_json::Value,
    ) -> (Disposition, EffectBatch) {
        self.tool_ledger
            .open(tool_call_id.clone(), tool_name.clone());
        let batch = EffectBatch::from_write(TranscriptWrite::ToolCall {
            tool_call_id,
            tool_name,
            input,
        });
        (
            Disposition::Continue,
            self.prepend_reasoning_flush(batch),
        )
    }

    /// Known open call → write only (projection in applier).
    /// Unknown id → notice only (emit without record), same as historical behavior.
    fn on_tool_result(
        &mut self,
        tool_call_id: ToolCallId,
        output: serde_json::Value,
        status: ToolResultStatus,
    ) -> (Disposition, EffectBatch) {
        if let Some(tool_name) = self.tool_ledger.close(&tool_call_id) {
            let batch = EffectBatch::from_write(TranscriptWrite::ToolResult {
                tool_call_id,
                tool_name,
                output,
                status,
            });
            (
                Disposition::Continue,
                self.prepend_reasoning_flush(batch),
            )
        } else {
            let batch = EffectBatch::from_notice(KernelEvent::GenerationToolResult {
                session_id: self.session_id().clone(),
                job_id: self.job.job_id.clone(),
                tool_call_id,
                output,
                status,
            });
            (
                Disposition::Continue,
                self.prepend_reasoning_flush(batch),
            )
        }
    }

    /// Seal still-open tools: durable Incomplete results only (projection in applier).
    /// Opt-in via [`ToolCallSealPolicy`] — the default posture is `LeaveOpen`.
    fn seal_incomplete_effects(&mut self) -> EffectBatch {
        let open = self.tool_ledger.seal_incomplete();
        let mut batch = EffectBatch::empty();
        if let Some(write) = self.take_reasoning_write() {
            batch.push_write(write);
        }
        if open.is_empty() {
            return batch;
        }
        let output = serde_json::json!({});
        for (tool_call_id, tool_name) in open {
            batch.push_write(TranscriptWrite::ToolResult {
                tool_call_id,
                tool_name,
                output: output.clone(),
                status: ToolResultStatus::Incomplete,
            });
        }
        batch
    }

    /// Own the agent stream loop: handle → [`EffectApplier::apply_stream`] → optional seal → outcome.
    pub(crate) async fn drive(
        mut self,
        stream: AgentEventStream,
        mut is_cancelled: impl FnMut() -> bool,
        agent_cancel: &TurnCancel,
        tool_call_seal: ToolCallSealPolicy,
        applier: &EffectApplier<'_>,
    ) -> Result<TurnOutcome> {
        let mut stream = stream;
        let mut stopped = false;
        let mut terminal: Option<TurnTerminal> = None;
        let mut stream_err: Option<String> = None;

        while let Some(item) = stream.next().await {
            if is_cancelled() {
                agent_cancel.cancel();
                stopped = true;
                break;
            }
            match item {
                Ok(event) => {
                    let (disposition, batch) = self.handle(event);
                    applier.commit(CommitUnit::Stream {
                        job_id: self.job_id(),
                        batch,
                    })?;
                    if let Disposition::Terminal(t) = disposition {
                        terminal = Some(t);
                        break;
                    }
                }
                Err(msg) => {
                    stream_err = Some(msg);
                    break;
                }
            }
        }

        // aborting = cooperative stop mid-stream or cancel-epoch stale after stream.
        // Abort outranks agent terminal / stream error (interrupt, not fault classification).
        let aborting = stopped || is_cancelled();
        // Flush any open CoT span before seal / terminal (durable sibling row).
        if !self.reasoning_buffer.is_empty() {
            let job_id = self.job_id().clone();
            let batch = self
                .take_reasoning_write()
                .map(EffectBatch::from_write)
                .unwrap_or_else(EffectBatch::empty);
            if !batch.is_empty() {
                applier.commit(CommitUnit::Stream {
                    job_id: &job_id,
                    batch,
                })?;
            }
        }
        if tool_call_seal.should_seal(aborting) {
            let job_id = self.job_id().clone();
            let batch = self.seal_incomplete_effects();
            if !batch.is_empty() {
                applier.commit(CommitUnit::Stream {
                    job_id: &job_id,
                    batch,
                })?;
            }
        }

        Ok(if aborting {
            // External stop/cancel → Aborted (partial buffer), not Failed.
            self.into_aborted()
        } else if let Some(TurnTerminal::Failed(msg)) = terminal {
            // AgentEvent::Error → Failed (error message).
            self.into_failed(msg)
        } else if let Some(msg) = stream_err {
            // Stream transport / adapter Err → Failed.
            self.into_failed(msg)
        } else {
            // TurnTerminal::Finished or clean end without terminal → Completed.
            self.into_completed()
        })
    }

    /// Normal success: buffer becomes assistant `content`.
    #[must_use]
    pub(crate) fn into_completed(self) -> TurnOutcome {
        TurnOutcome::Completed {
            job: self.job,
            assistant_message_id: self.assistant_message_id,
            content: self.buffer,
        }
    }

    /// Stop/cancel interrupt: buffer is `partial` (may be empty). See [`TurnOutcome`].
    #[must_use]
    pub(crate) fn into_aborted(self) -> TurnOutcome {
        TurnOutcome::Aborted {
            job: self.job,
            assistant_message_id: self.assistant_message_id,
            partial: self.buffer,
        }
    }

    /// Fault: no partial field — `message` only. See [`TurnOutcome`].
    #[must_use]
    pub(crate) fn into_failed(self, message: impl Into<String>) -> TurnOutcome {
        TurnOutcome::Failed {
            job: self.job,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::epoch::Epoch;
    use super::*;
    use crate::ids::MessageId;

    #[test]
    fn handle_text_delta_buffers_and_emits_without_side_channels() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        let (disposition, batch) = turn.handle(AgentEvent::TextDelta { text: "hi".into() });
        assert!(matches!(disposition, Disposition::Continue));
        assert_eq!(turn.buffer, "hi");
        assert!(batch.writes.is_empty());
        assert_eq!(batch.notices.len(), 1);
        assert!(matches!(
            batch.notices[0],
            KernelEvent::GenerationTextDelta { .. }
        ));
    }

    #[test]
    fn handle_finished_is_terminal() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        let (disposition, batch) = turn.handle(AgentEvent::Finished {
            reason: Some("stop".into()),
        });
        assert!(matches!(
            disposition,
            Disposition::Terminal(TurnTerminal::Finished)
        ));
        assert!(batch.is_empty());
    }

    #[test]
    fn handle_tool_call_opens_ledger_and_write_only() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        let (disposition, batch) = turn.handle(AgentEvent::ToolCall {
            tool_call_id: "tc1".into(),
            tool_name: "echo".into(),
            input: serde_json::json!({"x": 1}),
        });
        assert!(matches!(disposition, Disposition::Continue));
        // No dual-write: write only; projection happens in applier.
        assert!(batch.notices.is_empty());
        assert_eq!(batch.writes.len(), 1);
        assert!(matches!(
            &batch.writes[0],
            TranscriptWrite::ToolCall {
                tool_call_id,
                tool_name,
                ..
            } if tool_call_id.as_str() == "tc1" && tool_name.as_str() == "echo"
        ));
        // close known → name; unknown → None
        assert_eq!(
            turn.tool_ledger
                .close(&ToolCallId::new("tc1"))
                .as_ref()
                .map(ToolName::as_str),
            Some("echo")
        );
    }

    #[test]
    fn seal_incomplete_after_tool_call_without_result() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        let (_disposition, _batch) = turn.handle(AgentEvent::ToolCall {
            tool_call_id: "tc-open".into(),
            tool_name: "search".into(),
            input: serde_json::json!({}),
        });
        let seal = turn.seal_incomplete_effects();
        assert!(seal.notices.is_empty(), "projection only after apply");
        assert_eq!(seal.writes.len(), 1);
        assert!(matches!(
            &seal.writes[0],
            TranscriptWrite::ToolResult {
                tool_call_id,
                tool_name,
                status: ToolResultStatus::Incomplete,
                output,
                ..
            } if tool_call_id.as_str() == "tc-open"
                && tool_name.as_str() == "search"
                && output == &serde_json::json!({})
        ));
        assert!(turn.tool_ledger.is_empty());
    }

    /// Status field is sole outcome authority — kernel must not sniff `output` JSON keys.
    #[test]
    fn status_field_is_sole_authority_anti_sniff() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        let _ = turn.handle(AgentEvent::ToolCall {
            tool_call_id: "tc1".into(),
            tool_name: "echo".into(),
            input: serde_json::json!({"x": 1}),
        });
        let (disposition, batch) = turn.handle(AgentEvent::ToolResult {
            tool_call_id: "tc1".into(),
            // Misleading keys must NOT change outcome when status is Ok.
            output: serde_json::json!({"denied": true}),
            status: ToolResultStatus::Ok,
        });
        assert!(matches!(disposition, Disposition::Continue));
        assert!(batch.notices.is_empty());
        assert_eq!(batch.writes.len(), 1);
        assert!(matches!(
            &batch.writes[0],
            TranscriptWrite::ToolResult {
                status: ToolResultStatus::Ok,
                ..
            }
        ));
    }

    #[test]
    fn tool_result_explicit_denied_status() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        let _ = turn.handle(AgentEvent::ToolCall {
            tool_call_id: "tc1".into(),
            tool_name: "echo".into(),
            input: serde_json::json!({}),
        });
        let (disposition, batch) = turn.handle(AgentEvent::ToolResult {
            tool_call_id: "tc1".into(),
            output: serde_json::json!({}),
            status: ToolResultStatus::Denied,
        });
        assert!(matches!(disposition, Disposition::Continue));
        assert!(batch.notices.is_empty());
        assert!(matches!(
            &batch.writes[0],
            TranscriptWrite::ToolResult {
                status: ToolResultStatus::Denied,
                ..
            }
        ));
    }

    #[test]
    fn unknown_tool_result_is_notice_only() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let mut turn = GenerationTurn::begin(job, Epoch::ZERO, Vec::new());
        // No prior ToolCall — ledger has no open id.
        let (disposition, batch) = turn.handle(AgentEvent::ToolResult {
            tool_call_id: "orphan".into(),
            output: serde_json::json!({"ok": true}),
            status: ToolResultStatus::Ok,
        });
        assert!(matches!(disposition, Disposition::Continue));
        assert!(batch.writes.is_empty());
        assert_eq!(batch.notices.len(), 1);
        assert!(matches!(
            &batch.notices[0],
            KernelEvent::GenerationToolResult {
                tool_call_id,
                status: ToolResultStatus::Ok,
                ..
            } if tool_call_id.as_str() == "orphan"
        ));
    }

    #[test]
    fn start_effects_emits_generation_start() {
        let job = GenerationJob::new(SessionId::generate(), MessageId::generate());
        let turn = GenerationTurn::begin(job.clone(), Epoch::ZERO, Vec::new());
        let batch = turn.start_effects();
        assert!(batch.writes.is_empty());
        assert_eq!(batch.notices.len(), 1);
        assert!(matches!(
            &batch.notices[0],
            KernelEvent::GenerationStart {
                job_id,
                user_message_id,
                ..
            } if job_id == &job.job_id && user_message_id == &job.user_message_id
        ));
    }
}
