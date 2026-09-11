//! Transcript authority port + reference in-memory implementation.
//!
//! Write authority for session dialogue. No graph / edges / soft-delete.
//! SendQueue **records** outcomes here; it does not own history.
//!
//! | Module | Role |
//! |--------|------|
//! | (this) | [`Transcript`] trait + [`TranscriptRow`] + result types |
//! | [`memory`] | [`InMemoryTranscript`] — stores [`TranscriptRow`] directly |

mod memory;

pub use memory::InMemoryTranscript;

use serde_json::Value;

use crate::agent::{ToolCallId, ToolName, ToolResultStatus, TurnItem};
use crate::content::MessageContent;
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

/// One durable transcript row: stable [`MessageId`] + model material [`TurnItem`].
///
/// **Authority read shape** for products (document snapshot, fork copy).
/// [`TurnRequest::history`] stays [`Vec<TurnItem>`] — strip with
/// [`TranscriptRow::item`] / [`Transcript::load_turn_history`].
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptRow {
    pub id: MessageId,
    pub item: TurnItem,
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

    /// Load durable rows in transcript order (id + [`TurnItem`]).
    ///
    /// Primary read API for products that need stable message identities.
    fn load_rows(&self, session_id: &SessionId) -> Result<Vec<TranscriptRow>>;

    /// Model-facing history: same order as [`Self::load_rows`], items only.
    ///
    /// Default: map [`TranscriptRow::item`]. Implementors may override.
    fn load_turn_history(&self, session_id: &SessionId) -> Result<Vec<TurnItem>> {
        Ok(self
            .load_rows(session_id)?
            .into_iter()
            .map(|r| r.item)
            .collect())
    }

    /// Append a user message. Requires a live session.
    fn record_user(&self, session_id: &SessionId, content: &MessageContent)
    -> Result<RecordResult>;

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

    /// Append a reasoning / CoT sibling row ([`TurnItem::Reasoning`]).
    ///
    /// Requires a live session. Order is transcript order: typically before the
    /// answering assistant row, possibly interleaved with tool rows.
    fn record_reasoning(&self, session_id: &SessionId, content: &str) -> Result<RecordResult>;

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
