//! Session turn runner: spawn `AgentRuntime` stream → pollable progress for the TUI tick.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use futures::StreamExt;
use phi_kernel::{
    AgentEvent, AgentPrefix, AgentRuntime, JobId, SessionId, ToolCallSealPolicy, TurnCancel,
    TurnItem, TurnRequest,
};

/// One step of turn progress for the UI driver (not raw provider events).
#[derive(Debug, Clone)]
pub enum TurnProgress {
    TextDelta(String),
    Error(String),
    Finished,
}

/// Owns the active generation: starts async `AgentRuntime::run`, exposes non-blocking poll.
pub struct SessionTurnRunner {
    agent: Arc<dyn AgentRuntime>,
    session_id: SessionId,
    progress_rx: Option<Receiver<TurnProgress>>,
    busy: bool,
}

impl SessionTurnRunner {
    #[must_use]
    pub fn new(agent: Arc<dyn AgentRuntime>) -> Self {
        Self {
            agent,
            session_id: SessionId::generate(),
            progress_rx: None,
            busy: false,
        }
    }

    #[must_use]
    pub fn with_session(agent: Arc<dyn AgentRuntime>, session_id: SessionId) -> Self {
        Self {
            agent,
            session_id,
            progress_rx: None,
            busy: false,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Start a turn with full interleaved history (must already include the new user row).
    ///
    /// Spawns work on the current Tokio runtime. Returns `Err` if already busy.
    pub fn start_turn(&mut self, history: Vec<TurnItem>) -> Result<(), String> {
        if self.busy {
            return Err("turn already in progress".into());
        }
        if history.is_empty() {
            return Err("history is empty".into());
        }

        let (tx, rx) = mpsc::channel();
        self.progress_rx = Some(rx);
        self.busy = true;

        let agent = Arc::clone(&self.agent);
        let session_id = self.session_id.clone();
        let job_id = JobId::generate();
        let cancel = TurnCancel::new();
        let request = TurnRequest {
            session_id,
            job_id,
            history,
            prefix: AgentPrefix::baseline_chat(Vec::new()),
            tool_call_seal: ToolCallSealPolicy::LeaveOpen,
            cancel: cancel.clone(),
        };

        tokio::spawn(async move {
            let _cancel = cancel;
            match agent.run(request).await {
                Ok(mut stream) => {
                    let mut saw_terminal = false;
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(AgentEvent::TextDelta { text }) => {
                                if !text.is_empty()
                                    && tx.send(TurnProgress::TextDelta(text)).is_err()
                                {
                                    return;
                                }
                            }
                            Ok(AgentEvent::ReasoningDelta { text }) => {
                                // Surface CoT in the same stream for step-1 UX
                                // (DeepSeek responses emit reasoning before final text).
                                if !text.is_empty()
                                    && tx.send(TurnProgress::TextDelta(text)).is_err()
                                {
                                    return;
                                }
                            }
                            Ok(AgentEvent::Error { message }) => {
                                let _ = tx.send(TurnProgress::Error(message));
                                saw_terminal = true;
                                break;
                            }
                            Ok(AgentEvent::Finished { .. }) => {
                                saw_terminal = true;
                                break;
                            }
                            Ok(
                                AgentEvent::ToolCall { .. }
                                | AgentEvent::ToolResult { .. }
                                | AgentEvent::ToolApprovalRequired { .. }
                                | AgentEvent::Unknown { .. },
                            ) => {
                                // Step-1 text-only.
                            }
                            Err(e) => {
                                let _ = tx.send(TurnProgress::Error(e));
                                saw_terminal = true;
                                break;
                            }
                        }
                    }
                    if !saw_terminal {
                        // Stream ended without Finished/Error.
                    }
                    let _ = tx.send(TurnProgress::Finished);
                }
                Err(e) => {
                    let _ = tx.send(TurnProgress::Error(e));
                    let _ = tx.send(TurnProgress::Finished);
                }
            }
        });

        Ok(())
    }

    /// Non-blocking: drain available progress events.
    pub fn poll(&mut self) -> Vec<TurnProgress> {
        let Some(rx) = self.progress_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(p) => {
                    let finished = matches!(p, TurnProgress::Finished);
                    out.push(p);
                    if finished {
                        self.busy = false;
                        self.progress_rx = None;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !out.iter().any(|p| matches!(p, TurnProgress::Finished)) {
                        out.push(TurnProgress::Finished);
                    }
                    self.busy = false;
                    self.progress_rx = None;
                    break;
                }
            }
        }
        out
    }
}
