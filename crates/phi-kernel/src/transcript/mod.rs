//! Transcript authority port + reference in-memory implementation.
//!
//! Write authority for session dialogue. No graph / edges / soft-delete.
//! SendQueue **records** outcomes here; it does not own history.
//!
//! | Module | Role |
//! |--------|------|
//! | (this) | [`Transcript`] trait + [`TranscriptRow`] + result types |
//! | [`memory`] | [`InMemoryTranscript`] — stores [`TranscriptRow`] directly |

mod generation;
mod memory;

pub use memory::InMemoryTranscript;

use serde_json::Value;

use crate::GenerationJob;
use crate::agent::{
    ModelResponse, ToolArguments, ToolCallId, ToolName, ToolResultStatus, TurnItem,
};
use crate::content::MessageContent;
use crate::error::Result;
use crate::ids::{JobId, MessageId, ModelResponseId, SessionId};
use serde::{Deserialize, Serialize};

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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptRow {
    pub id: MessageId,
    pub item: TurnItem,
    pub generation: Option<GenerationStamp>,
}

impl TranscriptRow {
    pub fn new(id: MessageId, item: TurnItem) -> Self {
        Self {
            id,
            item,
            generation: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerationStamp {
    pub job_id: JobId,
    pub user_message_id: MessageId,
    pub response_id: ModelResponseId,
    pub response_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GenerationStatus {
    Pending,
    Running,
    Completed,
    Stopped,
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationRecord {
    pub job: GenerationJob,
    pub status: GenerationStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptSession {
    pub rows: Vec<TranscriptRow>,
    pub live: bool,
    pub generations: Vec<GenerationRecord>,
}

#[derive(Clone, Debug)]
pub enum GenerationCommit {
    Enqueue {
        job: GenerationJob,
    },
    Start {
        job: GenerationJob,
    },
    Response {
        job: GenerationJob,
        response: ModelResponse,
    },
    ToolResult {
        job: GenerationJob,
        response_id: ModelResponseId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        output: Value,
        status: ToolResultStatus,
    },
    Finish {
        job: GenerationJob,
        status: GenerationStatus,
    },
}

impl GenerationCommit {
    pub fn job(&self) -> &GenerationJob {
        match self {
            Self::Enqueue { job }
            | Self::Start { job }
            | Self::Response { job, .. }
            | Self::ToolResult { job, .. }
            | Self::Finish { job, .. } => job,
        }
    }
}

/// Write-authority port for session dialogue history.
///
/// The kernel reads and writes session dialogue through this trait and does not
/// care whether the backend is memory, SQLite, or a product store.
pub trait Transcript: Send + Sync {
    /// Must return only after the complete mutation has committed durably.
    fn commit_generation(&self, commit: &GenerationCommit) -> Result<u64>;
    fn session_snapshot(&self, session_id: &SessionId) -> Result<TranscriptSession>;

    /// Causal model history up to this input, independent of physical append order.
    fn load_job_history(&self, job: &GenerationJob) -> Result<Vec<TurnItem>> {
        let snapshot = self.session_snapshot(&job.session_id)?;
        snapshot.history_for(job)
    }
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
        input: &ToolArguments,
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
