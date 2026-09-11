//! Directory of per-session [`SendQueue`] objects + injected agent ports.
//!
//! **SRP:** find + deliver only. No graph, close policy, or product orchestration.
//! [`AgentPorts`] (runtime + materials) are constructor-injected (DIP); permission/approval
//! policy stays in the `AgentRuntime` adapter (ISP — directory holds no ACL port).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agent::AgentPorts;
use crate::error::{KernelError, Result};
use crate::events::EventBus;
use crate::ids::{JobId, SessionId};
use crate::transcript::Transcript;

use super::job::GenerationJob;
use super::queue::SendQueue;

/// `session_id → SendQueue` plus constructor-injected [`AgentPorts`].
///
/// [`Clone`] shares the directory map and injected ports (`Arc`); it does **not**
/// copy session tables or fork per-session queue state.
#[derive(Clone)]
pub struct SessionDirectory {
    sessions: Arc<Mutex<HashMap<String, SendQueue>>>,
    ports: AgentPorts,
}

impl SessionDirectory {
    /// Inject runtime + materials (see [`AgentPorts::from_sources`] for the common case).
    #[must_use]
    pub fn new(ports: AgentPorts) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            ports,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SendQueue>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Ensure a queue exists for `id` (eager warm). `enqueue` also lazy-creates.
    pub fn activate(&self, id: &SessionId) {
        let mut map = self.lock();
        map.entry(id.as_str().to_owned())
            .or_insert_with(|| SendQueue::new(id.clone()));
    }

    pub fn rebuild(&self, live_ids: &[SessionId]) {
        let mut map = self.lock();
        map.clear();
        for id in live_ids {
            map.insert(id.as_str().to_owned(), SendQueue::new(id.clone()));
        }
    }

    pub fn evict(&self, id: &SessionId) {
        let mut map = self.lock();
        if let Some(q) = map.remove(id.as_str()) {
            q.abort_turn_and_clear_queue();
        }
    }

    fn get(&self, id: &SessionId) -> Option<SendQueue> {
        self.lock().get(id.as_str()).cloned()
    }

    /// Push job; creates SendQueue if missing. Returns pending job ids.
    ///
    /// `job.session_id` must equal `session_id`.
    pub fn enqueue(&self, session_id: &SessionId, job: GenerationJob) -> Result<Vec<JobId>> {
        if &job.session_id != session_id {
            return Err(KernelError::InvalidArgument(format!(
                "job session {} does not match enqueue target {}",
                job.session_id, session_id
            )));
        }
        let mut map = self.lock();
        let q = map
            .entry(session_id.as_str().to_owned())
            .or_insert_with(|| SendQueue::new(session_id.clone()));
        q.enqueue(job)?;
        Ok(q.pending_ids())
    }

    pub fn stop(&self, session_id: &SessionId) {
        if let Some(q) = self.get(session_id) {
            q.abort_current_turn_only();
        }
    }
    pub fn pause_claim(&self, session_id: &SessionId) {
        self.activate(session_id);
        if let Some(queue) = self.get(session_id) {
            queue.pause_claims();
        }
    }
    pub fn resume_claim(&self, session_id: &SessionId) -> Result<()> {
        match self.get(session_id) {
            Some(queue) => queue.resume_claims(),
            None => Ok(()),
        }
    }
    pub async fn stop_and_wait(&self, session_id: &SessionId) -> Result<()> {
        match self.get(session_id) {
            Some(queue) => queue.stop_and_wait().await,
            None => Ok(()),
        }
    }

    /// Whether this session has neither a running generation nor pending work.
    #[must_use]
    pub fn is_idle(&self, session_id: &SessionId) -> bool {
        self.get(session_id).is_none_or(|queue| queue.is_idle())
    }

    pub fn cancel_pending(
        &self,
        session_id: &SessionId,
        job_id: Option<&JobId>,
    ) -> (usize, Vec<JobId>) {
        let Some(q) = self.get(session_id) else {
            return (0, Vec::new());
        };
        let n = if let Some(jid) = job_id {
            usize::from(q.cancel_pending_job(jid))
        } else {
            q.cancel_all_pending()
        };
        (n, q.pending_ids())
    }
    pub fn pending_ids(&self, session_id: &SessionId) -> Vec<JobId> {
        self.get(session_id)
            .map_or_else(Vec::new, |queue| queue.pending_ids())
    }
    pub fn fault(&self, session_id: &SessionId) -> Option<(JobId, String)> {
        self.get(session_id)?.fault()
    }

    /// Deliver pump only. Missing queue → `Ok(())`.
    pub async fn run_until_idle(
        &self,
        session_id: &SessionId,
        transcript: &dyn Transcript,
        bus: &EventBus,
    ) -> Result<()> {
        let Some(queue) = self.get(session_id) else {
            return Ok(());
        };
        queue.run_until_idle(transcript, &self.ports, bus).await
    }
}
