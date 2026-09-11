//! Kernel generation / queue observation events.
//!
//! **Not** product command events for graph close/fork. Use-case layers may
//! publish additional command-class events outside this bus if needed.
//!
//! **Naming:** do not call this a "mailbox" — that name is reserved for a future
//! agent async-notification mechanism.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::agent::{ToolCallId, ToolName, ToolResultStatus, Usage};
use crate::ids::{JobId, MessageId, SessionId};

/// Multi-subscriber fan-out for kernel observers (SSE / tests).
pub type EventBus = broadcast::Sender<KernelEvent>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KernelEvent {
    #[serde(rename_all = "camelCase")]
    GenerationModelResponseCommitted {
        session_id: SessionId,
        job_id: JobId,
        response_id: crate::ModelResponseId,
        assistant_message_ids: Vec<MessageId>,
        version: u64,
    },
    #[serde(rename_all = "camelCase")]
    GenerationStart {
        session_id: SessionId,
        job_id: JobId,
        user_message_id: MessageId,
        assistant_message_id: MessageId,
        version: u64,
    },
    #[serde(rename_all = "camelCase")]
    GenerationTextDelta {
        session_id: SessionId,
        job_id: JobId,
        assistant_message_id: MessageId,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    GenerationReasoningDelta {
        session_id: SessionId,
        job_id: JobId,
        assistant_message_id: MessageId,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    GenerationToolCall {
        session_id: SessionId,
        job_id: JobId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        input: Value,
    },
    #[serde(rename_all = "camelCase")]
    GenerationToolResult {
        session_id: SessionId,
        job_id: JobId,
        tool_call_id: ToolCallId,
        output: Value,
        status: ToolResultStatus,
    },
    #[serde(rename_all = "camelCase")]
    GenerationToolApprovalRequired {
        session_id: SessionId,
        job_id: JobId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        input: Value,
    },
    #[serde(rename_all = "camelCase")]
    GenerationAgentUnknown {
        session_id: SessionId,
        job_id: JobId,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
    },
    /// Provider-reported token usage for this job (notice only; not transcript).
    #[serde(rename_all = "camelCase")]
    GenerationUsage {
        session_id: SessionId,
        job_id: JobId,
        usage: Usage,
    },
    #[serde(rename_all = "camelCase")]
    GenerationDone {
        session_id: SessionId,
        job_id: JobId,
        assistant_message_id: MessageId,
        content: String,
        version: u64,
    },
    #[serde(rename_all = "camelCase")]
    GenerationStopped {
        session_id: SessionId,
        job_id: JobId,
        assistant_message_id: MessageId,
        partial: String,
        version: u64,
    },
    #[serde(rename_all = "camelCase")]
    GenerationError {
        session_id: SessionId,
        job_id: JobId,
        message: String,
        version: u64,
    },
}
