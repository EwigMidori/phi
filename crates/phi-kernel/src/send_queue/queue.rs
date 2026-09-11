//! # SendQueue — per-session generation control
//!
//! **Name:** SendQueue (not "mailbox" — reserved for future agent async notify).
//!
//! One Live session = one object. Messages: enqueue / stop / cancel / claim /
//! [`run_until_idle`](SendQueue::run_until_idle).
//!
//! - **enqueue** = push only; **run_until_idle** = sole worker (claim + drain)
//! - Cancel: [`QueueInner::cancel_epoch`] is this queue’s stop-generation number;
//!   claim snapshots it, stop bumps it, and any claim with an older number is void
//! - Records outcomes on [`Transcript`] via [`EffectApplier`] only
//! - Publishes only generation-class [`KernelEvent`]s (no product graph rules)
//!
//! **SRP:** queue owns job lifecycle + pump exclusivity. Stream interpretation
//! lives on [`GenerationTurn`](super::turn::GenerationTurn) (`drive`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::agent::{AgentPorts, TurnCancel};
use crate::error::{KernelError, Result};
use crate::events::EventBus;
use crate::ids::{JobId, SessionId};
use crate::transcript::Transcript;

use super::effect::{CommitUnit, EffectApplier, TurnOutcome};
use super::epoch::Epoch;
use super::job::GenerationJob;
use super::turn::GenerationTurn;

/// One claimed generation seat: job + claim-time stop-generation snapshot.
///
/// `Option<RunningClaim>` excludes illegal half-filled running state
/// (job without its claim epoch, or the reverse).
#[derive(Clone, Debug)]
struct RunningClaim {
    job: GenerationJob,
    /// `cancel_epoch` at claim time; void once live has been bumped past this.
    epoch: Epoch,
    cancel: TurnCancel,
}

struct QueueInner {
    id: SessionId,
    pending: VecDeque<GenerationJob>,
    /// Current claim seat (job + claim-time epoch together).
    running: Option<RunningClaim>,
    /// This queue’s stop-generation number: claim snapshots it; stop bumps it;
    /// any claim still holding an older number is cancelled.
    cancel_epoch: Epoch,
    /// Whether a `run_until_idle` worker already holds this queue: only one
    /// pump may drain at a time (`try_begin_pump` / `PumpSeat` drop).
    pump_active: bool,
}

/// Per-session send / generation queue + pump.
///
/// [`Clone`] shares the same queue (`Arc`); it does **not** fork pending/running state.
#[derive(Clone)]
pub struct SendQueue {
    inner: Arc<Mutex<QueueInner>>,
}

impl SendQueue {
    #[must_use]
    pub fn new(id: SessionId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(QueueInner {
                id,
                pending: VecDeque::new(),
                running: None,
                cancel_epoch: Epoch::ZERO,
                pump_active: false,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn id(&self) -> SessionId {
        self.lock().id.clone()
    }

    #[must_use]
    pub(crate) fn is_cancelled(&self, claimed: Epoch) -> bool {
        claimed.is_stale(self.lock().cancel_epoch)
    }

    /// Stop only the current turn; keep pending jobs.
    pub fn abort_current_turn_only(&self) {
        let mut g = self.lock();
        g.cancel_epoch = g.cancel_epoch.bump();
        if let Some(claim) = &g.running {
            claim.cancel.cancel();
        }
    }

    /// Abort turn and drop all pending jobs.
    pub fn abort_turn_and_clear_queue(&self) {
        let mut g = self.lock();
        g.cancel_epoch = g.cancel_epoch.bump();
        if let Some(claim) = &g.running {
            claim.cancel.cancel();
        }
        g.pending.clear();
    }

    fn try_begin_pump(&self) -> bool {
        let mut g = self.lock();
        if g.pump_active {
            return false;
        }
        g.pump_active = true;
        true
    }

    fn end_pump(&self) {
        self.lock().pump_active = false;
    }

    /// Push job only — claim only inside [`run_until_idle`](Self::run_until_idle).
    ///
    /// Rejects jobs whose `session_id` does not match this queue (invariant).
    pub fn enqueue(&self, job: GenerationJob) -> Result<()> {
        let mut g = self.lock();
        if job.session_id != g.id {
            return Err(KernelError::InvalidArgument(format!(
                "job session {} does not match queue {}",
                job.session_id, g.id
            )));
        }
        g.pending.push_back(job);
        Ok(())
    }

    fn claim_for_pump(&self) -> Option<(GenerationJob, Epoch, TurnCancel)> {
        let mut g = self.lock();
        debug_assert!(g.pump_active, "claim_for_pump without pump seat");
        if let Some(claim) = g.running.clone() {
            return Some((claim.job, claim.epoch, claim.cancel));
        }
        let job = g.pending.pop_front()?;
        let epoch = g.cancel_epoch;
        let cancel = TurnCancel::new();
        g.running = Some(RunningClaim {
            job: job.clone(),
            epoch,
            cancel: cancel.clone(),
        });
        Some((job, epoch, cancel))
    }

    fn mark_finished(&self, job_id: &JobId) {
        let mut g = self.lock();
        if g.running.as_ref().is_some_and(|c| &c.job.job_id == job_id) {
            g.running = None;
        }
    }

    pub fn cancel_pending_job(&self, job_id: &JobId) -> bool {
        let mut g = self.lock();
        let before = g.pending.len();
        g.pending.retain(|j| &j.job_id != job_id);
        before != g.pending.len()
    }

    pub fn cancel_all_pending(&self) -> usize {
        let mut g = self.lock();
        let n = g.pending.len();
        g.pending.clear();
        n
    }

    #[must_use]
    pub fn pending_ids(&self) -> Vec<JobId> {
        self.lock()
            .pending
            .iter()
            .map(|j| j.job_id.clone())
            .collect()
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        let g = self.lock();
        g.running.is_none() && g.pending.is_empty()
    }

    /// Sole worker: drain queue via [`AgentPorts`]; record on `Transcript`.
    ///
    /// Per job: claim → begin → [`EffectApplier`] → materials.prepare → `agent.run` →
    /// [`GenerationTurn::drive`] → mark_finished + [`EffectApplier::apply_terminal`].
    pub async fn run_until_idle(
        &self,
        transcript: &dyn Transcript,
        ports: &AgentPorts,
        bus: &EventBus,
    ) -> Result<()> {
        if !self.try_begin_pump() {
            return Ok(());
        }
        let _seat = PumpSeat { queue: self };
        let session_id = self.id();
        let applier = EffectApplier::new(&session_id, transcript, bus);

        loop {
            if !transcript.is_live(&session_id)? {
                self.abort_turn_and_clear_queue();
                break;
            }

            let Some((job, claimed_epoch, cancel)) = self.claim_for_pump() else {
                break;
            };

            // Cancel fence stale before work → Aborted (interrupt), not Failed.
            if self.is_cancelled(claimed_epoch) {
                let turn = GenerationTurn::begin(job, claimed_epoch, Vec::new());
                self.finish_outcome(turn.into_aborted(), &applier)?;
                continue;
            }

            let history = transcript.load_turn_history(&session_id)?;
            let turn = GenerationTurn::begin(job, claimed_epoch, history);
            applier.commit(CommitUnit::Stream {
                job_id: turn.job_id(),
                batch: turn.start_effects(),
            })?;

            // Precondition fault → Failed (error message), not Aborted.
            if turn.history().is_empty() {
                self.finish_outcome(turn.into_failed("no dialogue for agent"), &applier)?;
                continue;
            }

            if self.is_cancelled(turn.claimed_epoch()) {
                self.finish_outcome(turn.into_aborted(), &applier)?;
                continue;
            }

            let request = ports.materials.prepare(
                &session_id,
                turn.job_id().clone(),
                turn.history().to_vec(),
                cancel.clone(),
            );
            let tool_call_seal = request.tool_call_seal;

            // Upload/request preparation belongs to run and can itself block.
            // Stop wakes this wait, releases the adapter future, and stays Aborted.
            let started = tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                result = ports.agent.run(request) => Some(result),
            };
            if cancel.is_cancelled() || self.is_cancelled(turn.claimed_epoch()) {
                self.finish_outcome(turn.into_aborted(), &applier)?;
                continue;
            }
            let Some(started) = started else {
                self.finish_outcome(turn.into_aborted(), &applier)?;
                continue;
            };
            let stream = match started {
                Ok(s) => s,
                Err(error) => {
                    self.finish_outcome(turn.into_failed(error), &applier)?;
                    continue;
                }
            };

            let claimed = turn.claimed_epoch();
            let outcome = turn
                .drive(
                    stream,
                    || self.is_cancelled(claimed),
                    &cancel,
                    tool_call_seal,
                    &applier,
                )
                .await?;
            self.finish_outcome(outcome, &applier)?;
        }
        Ok(())
    }

    /// Queue job seat + sole observation/transcript commit for terminal outcome.
    fn finish_outcome(&self, outcome: TurnOutcome, applier: &EffectApplier<'_>) -> Result<()> {
        self.mark_finished(outcome.job_id());
        applier.commit(CommitUnit::Terminal(outcome))
    }
}

struct PumpSeat<'a> {
    queue: &'a SendQueue,
}

impl Drop for PumpSeat<'_> {
    fn drop(&mut self) {
        self.queue.end_pump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MessageId;
    use crate::{
        AgentEvent, AgentEventStream, AgentRuntime, EmptyAgentPrefix, InMemoryTranscript,
        KernelEvent, MessageContent, TurnRequest,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[test]
    fn enqueue_does_not_claim_until_pump() {
        let sid = SessionId::generate();
        let q = SendQueue::new(sid.clone());
        let job = GenerationJob::new(sid, MessageId::generate());
        let jid = job.job_id.clone();
        q.enqueue(job).unwrap();
        assert_eq!(q.pending_ids(), vec![jid]);
    }

    #[test]
    fn enqueue_rejects_foreign_session() {
        let q = SendQueue::new(SessionId::generate());
        let foreign = GenerationJob::new(SessionId::generate(), MessageId::generate());
        assert!(q.enqueue(foreign).is_err());
    }

    #[test]
    fn pump_seat_exclusive() {
        let q = SendQueue::new(SessionId::generate());
        assert!(q.try_begin_pump());
        assert!(!q.try_begin_pump());
        q.end_pump();
        assert!(q.try_begin_pump());
        q.end_pump();
    }

    #[test]
    fn clear_queue_keeps_old_pump_seat_until_it_exits() {
        let sid = SessionId::generate();
        let q = SendQueue::new(sid.clone());
        q.enqueue(GenerationJob::new(sid.clone(), MessageId::generate()))
            .unwrap();
        assert!(q.try_begin_pump());
        let (old, _, cancel) = q.claim_for_pump().unwrap();
        q.abort_turn_and_clear_queue();
        assert!(cancel.is_cancelled());
        assert!(
            !q.try_begin_pump(),
            "aborting must not release another worker's seat"
        );
        assert!(!q.is_idle());
        q.mark_finished(&old.job_id);
        q.end_pump();
        q.enqueue(GenerationJob::new(sid, MessageId::generate()))
            .unwrap();
        assert!(q.try_begin_pump());
        let (new, _, fresh_cancel) = q.claim_for_pump().unwrap();
        assert!(!fresh_cancel.is_cancelled());
        q.mark_finished(&old.job_id);
        assert_eq!(q.lock().running.as_ref().unwrap().job.job_id, new.job_id);
    }

    struct WaitingFirstRun {
        preparing: bool,
        started: Arc<Notify>,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AgentRuntime for WaitingFirstRun {
        async fn run(
            &self,
            _request: TurnRequest,
        ) -> std::result::Result<AgentEventStream, String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                if self.preparing {
                    return futures::future::pending().await;
                }
                return Ok(Box::pin(futures::stream::pending()));
            }
            Ok(Box::pin(futures::stream::iter([
                Ok(AgentEvent::TextDelta {
                    text: "second completed".into(),
                }),
                Ok(AgentEvent::Finished { reason: None }),
            ])))
        }
    }

    async fn stop_waiting_turn(preparing: bool) {
        let sid = SessionId::generate();
        let transcript = InMemoryTranscript::new();
        transcript.ensure_live(&sid).unwrap();
        let user = transcript
            .record_user(&sid, &MessageContent::text("input"))
            .unwrap();
        let queue = SendQueue::new(sid.clone());
        let first = GenerationJob::new(sid.clone(), user.message_id.clone());
        let second = GenerationJob::new(sid.clone(), user.message_id);
        queue.enqueue(first.clone()).unwrap();
        queue.enqueue(second.clone()).unwrap();
        let started = Arc::new(Notify::new());
        let ports = AgentPorts::from_sources(
            Arc::new(WaitingFirstRun {
                preparing,
                started: started.clone(),
                calls: AtomicUsize::new(0),
            }),
            Arc::new(EmptyAgentPrefix),
            Arc::new(crate::FixedToolCallSeal(
                crate::ToolCallSealPolicy::LeaveOpen,
            )),
        );
        let (bus, mut events) = tokio::sync::broadcast::channel(32);
        let pump = queue.run_until_idle(&transcript, &ports, &bus);
        let stop = async {
            started.notified().await;
            queue.abort_current_turn_only();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(pump, stop)
        })
        .await
        .expect("stop must wake a pending adapter future or idle stream");
        result.unwrap();
        assert!(queue.is_idle());
        let history = transcript.load_turn_history(&sid).unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            matches!(&history[1], crate::TurnItem::Assistant { content } if content == "second completed")
        );
        let mut stopped = Vec::new();
        let mut done = Vec::new();
        while let Ok(event) = events.try_recv() {
            match event {
                KernelEvent::GenerationStopped { job_id, .. } => stopped.push(job_id),
                KernelEvent::GenerationDone { job_id, .. } => done.push(job_id),
                KernelEvent::GenerationError { .. } => {
                    panic!("cancellation must not become a fault")
                }
                _ => {}
            }
        }
        assert_eq!(stopped, vec![first.job_id]);
        assert_eq!(done, vec![second.job_id]);
    }

    #[tokio::test]
    async fn stop_interrupts_run_preparation_and_preserves_pending_jobs() {
        stop_waiting_turn(true).await;
    }

    #[tokio::test]
    async fn stop_interrupts_silent_stream_and_preserves_pending_jobs() {
        stop_waiting_turn(false).await;
    }
}
