//! Session turn runner: spawn `AgentRuntime` stream → pollable [`AgentEvent`]s for the TUI tick.
//!
//! No parallel progress enum — the channel carries kernel [`AgentEvent`] so tools /
//! approvals later need no second wire format.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use futures::StreamExt;
use phi_kernel::{
    AgentEvent, AgentPrefix, AgentRuntime, JobId, SessionId, ToolCallSealPolicy, TurnCancel,
    TurnItem, TurnRequest,
};

/// Owns the active generation: starts async `AgentRuntime::run`, exposes non-blocking poll.
pub struct SessionTurnRunner {
    agent: Arc<dyn AgentRuntime>,
    session_id: SessionId,
    event_rx: Option<Receiver<AgentEvent>>,
    busy: bool,
}

impl SessionTurnRunner {
    #[must_use]
    pub fn new(agent: Arc<dyn AgentRuntime>) -> Self {
        Self {
            agent,
            session_id: SessionId::generate(),
            event_rx: None,
            busy: false,
        }
    }

    #[must_use]
    pub fn with_session(agent: Arc<dyn AgentRuntime>, session_id: SessionId) -> Self {
        Self {
            agent,
            session_id,
            event_rx: None,
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
        self.event_rx = Some(rx);
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
                    let mut saw_finished = false;
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(ev) => {
                                let is_finished = matches!(ev, AgentEvent::Finished { .. });
                                if tx.send(ev).is_err() {
                                    return;
                                }
                                if is_finished {
                                    saw_finished = true;
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(AgentEvent::Error { message: e });
                                break;
                            }
                        }
                    }
                    if !saw_finished {
                        // Stream ended without Finished (error path or abrupt close).
                        let _ = tx.send(AgentEvent::Finished { reason: None });
                    }
                }
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error { message: e });
                    let _ = tx.send(AgentEvent::Finished { reason: None });
                }
            }
        });

        Ok(())
    }

    /// Non-blocking: drain available kernel events.
    pub fn poll(&mut self) -> Vec<AgentEvent> {
        let Some(rx) = self.event_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    let finished = matches!(ev, AgentEvent::Finished { .. });
                    out.push(ev);
                    if finished {
                        self.busy = false;
                        self.event_rx = None;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !out
                        .iter()
                        .any(|e| matches!(e, AgentEvent::Finished { .. }))
                    {
                        out.push(AgentEvent::Finished { reason: None });
                    }
                    self.busy = false;
                    self.event_rx = None;
                    break;
                }
            }
        }
        out
    }
}
