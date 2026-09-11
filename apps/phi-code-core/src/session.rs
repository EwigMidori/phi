//! Product session host: kernel [`SendQueue`] + [`Transcript`] + [`EventBus`].
//!
//! Generation is exclusively driven by the kernel pump; the product only
//! enqueues and observes. Pump failures stay on the host (no forged bus events).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use phi_kernel::{
    AgentPorts, AgentPrefixSource, AgentRuntime, EmptyAgentPrefix, EventBus, FixedToolCallSeal,
    GenerationJob, InMemoryTranscript, KernelEvent, SendQueue, SessionId, ToolCallSealPolicy,
    Transcript, TurnItem,
};
use tokio::sync::broadcast;

/// Result of a non-blocking event poll.
#[derive(Debug, Default)]
pub struct PollBatch {
    pub events: Vec<KernelEvent>,
    /// Receiver lagged — view must resync from [`SessionHost::history`].
    pub lagged: bool,
    /// Pump / `run_until_idle` failure (not a kernel bus event).
    pub pump_error: Option<String>,
}

/// Thin host over one live session: record user → enqueue → pump → poll bus.
pub struct SessionHost {
    session_id: SessionId,
    transcript: InMemoryTranscript,
    queue: SendQueue,
    ports: AgentPorts,
    bus: EventBus,
    rx: broadcast::Receiver<KernelEvent>,
    /// True while a pump task is outstanding (owns the drain loop).
    pump_running: Arc<AtomicBool>,
    /// Last pump-level error (mutex: written by pump task, taken by poll).
    pump_error: Arc<Mutex<Option<String>>>,
}

impl SessionHost {
    /// Build with kernel materials defaults (empty prefix, leave-open seal).
    #[must_use]
    pub fn new(agent: Arc<dyn AgentRuntime>) -> Self {
        Self::with_ports(AgentPorts::from_sources(
            agent,
            Arc::new(EmptyAgentPrefix) as Arc<dyn AgentPrefixSource>,
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::LeaveOpen)),
        ))
    }

    #[must_use]
    pub fn with_ports(ports: AgentPorts) -> Self {
        let session_id = SessionId::generate();
        let transcript = InMemoryTranscript::new();
        let _ = transcript.ensure_live(&session_id);
        let queue = SendQueue::new(session_id.clone());
        let (bus, rx) = broadcast::channel(512);
        Self {
            session_id,
            transcript,
            queue,
            ports,
            bus,
            rx,
            pump_running: Arc::new(AtomicBool::new(false)),
            pump_error: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.pump_running.load(Ordering::SeqCst) || !self.queue.pending_ids().is_empty()
    }

    /// Durable history from the kernel transcript.
    pub fn history(&self) -> Result<Vec<TurnItem>, String> {
        self.transcript
            .load_rows(&self.session_id)
            .map(|rows| {
                rows.into_iter()
                    .filter(|row| !matches!(row.item, TurnItem::Continuation { .. }))
                    .map(|row| row.item)
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    /// Record user text, enqueue generation, ensure a pump is running.
    pub fn submit_user(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty user text".into());
        }
        if self.is_busy() {
            return Err("generation already in progress".into());
        }
        let rec = self
            .transcript
            .record_user(&self.session_id, &phi_kernel::MessageContent::text(text))
            .map_err(|e| e.to_string())?;
        if !rec.wrote {
            return Err("transcript refused user write".into());
        }
        self.queue
            .enqueue(GenerationJob::new(self.session_id.clone(), rec.message_id))
            .map_err(|e| e.to_string())?;
        self.ensure_pump();
        Ok(())
    }

    /// Start a pump task if none is running. Safe under concurrent enqueue.
    fn ensure_pump(&self) {
        // Only one task owns `pump_running == true`.
        if self
            .pump_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let queue = self.queue.clone();
        let transcript = self.transcript.clone();
        let ports = self.ports.clone();
        let bus = self.bus.clone();
        let flag = Arc::clone(&self.pump_running);
        let pump_error = Arc::clone(&self.pump_error);
        tokio::spawn(async move {
            loop {
                if let Err(e) = queue.run_until_idle(&transcript, &ports, &bus).await {
                    // Host-private note only — never forge KernelEvent / JobId.
                    if let Ok(mut slot) = pump_error.lock() {
                        *slot = Some(format!("pump: {e}"));
                    }
                }
                // Keep draining while work remains (flag stays true).
                if !queue.pending_ids().is_empty() {
                    continue;
                }
                // Release seat, then re-check (close TOCTOU with submit/ensure_pump).
                flag.store(false, Ordering::SeqCst);
                if queue.pending_ids().is_empty() {
                    break;
                }
                // Job arrived after empty check / before or after store false.
                if flag
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    continue;
                }
                // Another ensure_pump won the seat.
                break;
            }
        });
    }

    /// Non-blocking drain of kernel observation events (+ host pump errors).
    pub fn poll_events(&mut self) -> PollBatch {
        let mut out = PollBatch::default();
        if let Ok(mut slot) = self.pump_error.lock() {
            out.pump_error = slot.take();
        }
        loop {
            match self.rx.try_recv() {
                Ok(ev) => out.events.push(ev),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    out.lagged = true;
                    // Discard backlog; caller must resync from transcript.
                    while let Ok(_) = self.rx.try_recv() {}
                    break;
                }
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
        out
    }
}
