//! Process-local [`Transcript`](super::Transcript) for tests and scaffold consumers.
//!
//! **Not** a product store schema. Tool rows are stored as ad-hoc JSON in `content`
//! (private to this module) — production document authority uses typed metadata.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::{DialogueRole, DialogueTurn, ToolCallId, ToolName, ToolResultStatus};
use crate::error::{KernelError, Result};
use crate::ids::{MessageId, SessionId};

use super::{RecordResult, Transcript, TruncateResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoredRole {
    User,
    Assistant,
    ToolCall,
    ToolResult,
}

#[derive(Clone, Debug)]
struct StoredMessage {
    id: MessageId,
    role: StoredRole,
    content: String,
}

struct SessionState {
    live: bool,
    messages: Vec<StoredMessage>,
}

struct MemoryInner {
    version: u64,
    sessions: HashMap<String, SessionState>,
}

/// In-memory transcript authority (reference implementation of [`Transcript`]).
///
/// Baseline store: user / assistant / tool rows only.
/// [`DialogueRole::System`](DialogueRole::System) is contract-reserved; not stored here yet.
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

    /// Require a live session; on success append `message` and bump version.
    fn append_live(&self, session_id: &SessionId, message: StoredMessage) -> Result<RecordResult> {
        let mut g = self.lock();
        let Some(s) = g.sessions.get_mut(session_id.as_str()) else {
            return Err(KernelError::SessionNotFound(session_id.to_string()));
        };
        if !s.live {
            return Err(KernelError::SessionNotLive(session_id.to_string()));
        }
        let message_id = message.id.clone();
        s.messages.push(message);
        g.version = g.version.saturating_add(1);
        Ok(RecordResult {
            message_id,
            version: g.version,
            wrote: true,
        })
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
        s.messages.push(StoredMessage {
            id: assistant_message_id.clone(),
            role: StoredRole::Assistant,
            content: content.to_owned(),
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

    fn load_dialogue(&self, session_id: &SessionId) -> Result<Vec<DialogueTurn>> {
        let g = self.lock();
        let Some(s) = g.sessions.get(session_id.as_str()) else {
            return Err(KernelError::SessionNotFound(session_id.to_string()));
        };
        Ok(s.messages
            .iter()
            .filter_map(|m| match m.role {
                StoredRole::User => Some(DialogueTurn {
                    role: DialogueRole::User,
                    content: m.content.clone(),
                }),
                StoredRole::Assistant => Some(DialogueTurn {
                    role: DialogueRole::Assistant,
                    content: m.content.clone(),
                }),
                StoredRole::ToolCall | StoredRole::ToolResult => None,
            })
            .collect())
    }

    fn record_user(&self, session_id: &SessionId, text: &str) -> Result<RecordResult> {
        self.append_live(
            session_id,
            StoredMessage {
                id: MessageId::generate(),
                role: StoredRole::User,
                content: text.to_owned(),
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

    fn record_tool_call(
        &self,
        session_id: &SessionId,
        tool_call_id: &ToolCallId,
        tool_name: &ToolName,
        input: &Value,
    ) -> Result<RecordResult> {
        // Ad-hoc JSON blob for this reference store only — not a cross-system schema.
        let content = serde_json::json!({
            "toolCallId": tool_call_id.as_str(),
            "toolName": tool_name.as_str(),
            "input": input,
        })
        .to_string();
        self.append_live(
            session_id,
            StoredMessage {
                id: MessageId::generate(),
                role: StoredRole::ToolCall,
                content,
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
        let content = serde_json::json!({
            "toolCallId": tool_call_id.as_str(),
            "toolName": tool_name.as_str(),
            "output": output,
            "status": status.as_ref(),
        })
        .to_string();
        self.append_live(
            session_id,
            StoredMessage {
                id: MessageId::generate(),
                role: StoredRole::ToolResult,
                content,
            },
        )
    }
}
