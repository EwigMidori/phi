//! One pending generation job after a user message is already in the transcript.
//!
//! [`SendJob`] is intentionally a **POD / work ticket** (correlation ids only).
//! Lifecycle and execution live elsewhere — do not grow this into a rich object.

use crate::ids::{JobId, MessageId, SessionId};

/// Correlation ticket for one pending generation (not a lifecycle owner).
///
/// **Shape constraint:** keep this a plain value bag (`job_id` / `session_id` /
/// `user_message_id` + `new`). Do **not** add queue/pump/stream/turn behavior
/// here — that belongs on [`super::SendQueue`] and
/// [`super::turn::GenerationTurn`]. Equality, if ever needed, is by `job_id`
/// alone, not structural field equality of the whole ticket.
#[derive(Clone, Debug)]
pub struct SendJob {
    pub job_id: JobId,
    pub session_id: SessionId,
    pub user_message_id: MessageId,
}

impl SendJob {
    #[must_use]
    pub fn new(session_id: SessionId, user_message_id: MessageId) -> Self {
        Self {
            job_id: JobId::generate(),
            session_id,
            user_message_id,
        }
    }
}
