//! Transcript authority port + reference in-memory implementation.
//!
//! Write authority for session dialogue. No graph / edges / soft-delete.
//! SendQueue **records** outcomes here; it does not own history.
//!
//! | Module | Role |
//! |--------|------|
//! | (this) | [`Transcript`] trait + shared result types |
//! | [`memory`] | [`InMemoryTranscript`] — tests / scaffold only |

mod memory;

pub use memory::InMemoryTranscript;

use serde_json::Value;

use crate::agent::{DialogueTurn, ToolCallId, ToolName, ToolResultStatus};
use crate::error::Result;
use crate::ids::{MessageId, SessionId};

#[derive(Clone, Debug)]
pub struct RecordResult {
    pub message_id: MessageId,
    pub version: u64,
    /// False when the session was not live (or write was skipped).
    pub wrote: bool,
}

/// Result of [`Transcript::truncate_from`].
#[derive(Clone, Debug)]
pub struct TruncateResult {
    pub version: u64,
    /// Number of message rows removed (anchor + suffix).
    pub removed_count: usize,
}

/// Write-authority port for session dialogue history.
///
/// The kernel reads and writes session dialogue through this trait and does not
/// care whether the backend is memory, SQLite, or a product store.
pub trait Transcript: Send + Sync {
    /// Ensure a live session exists (idempotent). Creates the session if missing.
    fn ensure_live(&self, session_id: &SessionId) -> Result<()>;

    /// Whether the session exists and is live (accepts further writes / pump work).
    fn is_live(&self, session_id: &SessionId) -> Result<bool>;

    /// Store-wide monotonic revision counter (not per-session).
    ///
    /// Advances on successful `record_*` (`wrote: true`) and [`Self::truncate_from`].
    /// Does not advance on reads, no-op [`Self::ensure_live`], or soft-skipped
    /// assistant writes (`wrote: false`).
    fn version(&self) -> Result<u64>;

    /// Load dialogue turns for the agent context (user / assistant; not tool rows).
    fn load_dialogue(&self, session_id: &SessionId) -> Result<Vec<DialogueTurn>>;

    /// Append a user message. Requires a live session.
    fn record_user(&self, session_id: &SessionId, text: &str) -> Result<RecordResult>;

    /// Delete the anchor message and every later message in that session (by order).
    ///
    /// Earlier messages are unchanged. Advances [`Self::version`]. Requires a live session.
    /// Anchor must exist in the session; role is not restricted here (Edit use-cases
    /// typically pass a user message id).
    fn truncate_from(
        &self,
        session_id: &SessionId,
        from_message_id: &MessageId,
    ) -> Result<TruncateResult>;

    /// Append an assistant message under the given id.
    ///
    /// Soft path: when the session is missing or not live, returns
    /// [`RecordResult::wrote`] = `false` instead of erroring.
    fn record_assistant(
        &self,
        session_id: &SessionId,
        assistant_message_id: &MessageId,
        content: &str,
    ) -> Result<RecordResult>;

    /// Append a tool-call row. Requires a live session.
    fn record_tool_call(
        &self,
        session_id: &SessionId,
        tool_call_id: &ToolCallId,
        tool_name: &ToolName,
        input: &Value,
    ) -> Result<RecordResult>;

    /// Append a tool-result row. Requires a live session.
    fn record_tool_result(
        &self,
        session_id: &SessionId,
        tool_call_id: &ToolCallId,
        tool_name: &ToolName,
        output: &Value,
        status: ToolResultStatus,
    ) -> Result<RecordResult>;
}
