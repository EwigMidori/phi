//! Process-local [`Transcript`](super::Transcript) for tests and scaffold consumers.
//!
//! Internal rows are the public authority shape [`TranscriptRow`] — no parallel
//! `StoredMessage` / role enum. Not a product SQLite schema.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::{ToolCallId, ToolName, ToolResultStatus, TurnItem};
use crate::error::{KernelError, Result};
use crate::ids::{MessageId, SessionId};

use super::{RecordResult, Transcript, TranscriptRow, TruncateResult};

struct SessionState {
    live: bool,
    /// Durable rows — same type products load via [`Transcript::load_rows`].
    messages: Vec<TranscriptRow>,
}

struct MemoryInner {
    version: u64,
    sessions: HashMap<String, SessionState>,
}

/// In-memory transcript authority (reference implementation of [`Transcript`]).
///
/// Baseline store: [`TranscriptRow`] sequence per session.
///
/// [`Clone`] shares the in-memory store (`Arc`); it does **not** snapshot-fork
/// sessions or message history.
#[derive(Clone)]
pub struct InMemoryTranscript {
    inner: Arc<Mutex<MemoryInner>>,
}

impl InMemoryTranscript {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryInner {
                version: 0,
                sessions: HashMap::new(),
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Mark session not live (pump must stop). Does not delete history.
    pub fn mark_not_live(&self, session_id: &SessionId) -> Result<()> {
        let mut g = self.lock();
        let Some(s) = g.sessions.get_mut(session_id.as_str()) else {
            return Err(KernelError::SessionNotFound(session_id.to_string()));
        };
        s.live = false;
        g.version = g.version.saturating_add(1);
        Ok(())
    }

    /// Require a live session; on success append `row` and bump version.
    fn append_live(&self, session_id: &SessionId, row: TranscriptRow) -> Result<RecordResult> {
        let mut g = self.lock();
        let Some(s) = g.sessions.get_mut(session_id.as_str()) else {
            return Err(KernelError::SessionNotFound(session_id.to_string()));
        };
        if !s.live {
            return Err(KernelError::SessionNotLive(session_id.to_string()));
        }
        let message_id = row.id.clone();
        s.messages.push(row);
        g.version = g.version.saturating_add(1);
        Ok(RecordResult {
            message_id,
            version: g.version,
            wrote: true,
        })
    }

    /// Store-adapter rehydrate: replace one session's durable rows, preserving
    /// message ids. Does **not** advance [`Transcript::version`] (product
    /// document clock is separate). Overwrites any prior state for `session_id`.
    pub fn rehydrate_session(
        &self,
        session_id: &SessionId,
        rows: Vec<TranscriptRow>,
        live: bool,
    ) -> Result<()> {
        let mut g = self.lock();
        g.sessions.insert(
            session_id.as_str().to_owned(),
            SessionState {
                live,
                messages: rows,
            },
        );
        Ok(())
    }

    /// Soft write path for assistant: skip when missing / not live (`wrote: false`).
    fn try_append_assistant(
        &self,
        session_id: &SessionId,
        assistant_message_id: &MessageId,
        content: &str,
    ) -> Result<RecordResult> {
        let mut g = self.lock();
        let Some(s) = g.sessions.get_mut(session_id.as_str()) else {
            return Ok(RecordResult {
                message_id: assistant_message_id.clone(),
                version: g.version,
                wrote: false,
            });
        };
        if !s.live {
            return Ok(RecordResult {
                message_id: assistant_message_id.clone(),
                version: g.version,
                wrote: false,
            });
        }
        s.messages.push(TranscriptRow {
            id: assistant_message_id.clone(),
            item: TurnItem::Assistant {
                content: content.to_owned(),
            },
        });
        g.version = g.version.saturating_add(1);
        Ok(RecordResult {
            message_id: assistant_message_id.clone(),
            version: g.version,
            wrote: true,
        })
    }
}

impl Default for InMemoryTranscript {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcript for InMemoryTranscript {
    fn ensure_live(&self, session_id: &SessionId) -> Result<()> {
        let mut g = self.lock();
        g.sessions
            .entry(session_id.as_str().to_owned())
            .or_insert_with(|| SessionState {
                live: true,
                messages: Vec::new(),
            })
            .live = true;
        Ok(())
    }

    fn is_live(&self, session_id: &SessionId) -> Result<bool> {
        Ok(self
            .lock()
            .sessions
            .get(session_id.as_str())
            .is_some_and(|s| s.live))
    }

    fn version(&self) -> Result<u64> {
        Ok(self.lock().version)
    }

    fn load_rows(&self, session_id: &SessionId) -> Result<Vec<TranscriptRow>> {
        let g = self.lock();
        let Some(s) = g.sessions.get(session_id.as_str()) else {
            return Err(KernelError::SessionNotFound(session_id.to_string()));
        };
        Ok(s.messages.clone())
    }

    fn record_user(
        &self,
        session_id: &SessionId,
        content: &crate::MessageContent,
    ) -> Result<RecordResult> {
        self.append_live(
            session_id,
            TranscriptRow {
                id: MessageId::generate(),
                item: TurnItem::User {
                    content: content.clone(),
                },
            },
        )
    }

    fn truncate_from(
        &self,
        session_id: &SessionId,
        from_message_id: &MessageId,
    ) -> Result<TruncateResult> {
        let mut g = self.lock();
        let Some(s) = g.sessions.get_mut(session_id.as_str()) else {
            return Err(KernelError::SessionNotFound(session_id.to_string()));
        };
        if !s.live {
            return Err(KernelError::SessionNotLive(session_id.to_string()));
        }
        let idx = s
            .messages
            .iter()
            .position(|m| &m.id == from_message_id)
            .ok_or_else(|| {
                KernelError::InvalidArgument(format!(
                    "message not found in session: {from_message_id}"
                ))
            })?;
        let removed_count = s.messages.len() - idx;
        s.messages.truncate(idx);
        g.version = g.version.saturating_add(1);
        Ok(TruncateResult {
            version: g.version,
            removed_count,
        })
    }

    fn record_assistant(
        &self,
        session_id: &SessionId,
        assistant_message_id: &MessageId,
        content: &str,
    ) -> Result<RecordResult> {
        self.try_append_assistant(session_id, assistant_message_id, content)
    }

    fn record_reasoning(&self, session_id: &SessionId, content: &str) -> Result<RecordResult> {
        self.append_live(
            session_id,
            TranscriptRow {
                id: MessageId::generate(),
                item: TurnItem::Reasoning {
                    content: content.to_owned(),
                },
            },
        )
    }

    fn record_tool_call(
        &self,
        session_id: &SessionId,
        tool_call_id: &ToolCallId,
        tool_name: &ToolName,
        input: &Value,
    ) -> Result<RecordResult> {
        self.append_live(
            session_id,
            TranscriptRow {
                id: MessageId::generate(),
                item: TurnItem::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                },
            },
        )
    }

    fn record_tool_result(
        &self,
        session_id: &SessionId,
        tool_call_id: &ToolCallId,
        tool_name: &ToolName,
        output: &Value,
        status: ToolResultStatus,
    ) -> Result<RecordResult> {
        self.append_live(
            session_id,
            TranscriptRow {
                id: MessageId::generate(),
                item: TurnItem::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    output: output.clone(),
                    status,
                },
            },
        )
    }
}
