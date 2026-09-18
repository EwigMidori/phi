//! # phi-kernel
//!
//! Tree-optional **agent kernel** for [phi](https://github.com/ewigmidori/phi):
//!
//! | Area | Surface |
//! |------|---------|
//! | Contract | [`AgentRuntime`], [`TurnRequest`], [`AgentEvent`] |
//! | Generation | [`SendQueue`], [`SessionDirectory`], [`GenerationJob`] |
//! | History | [`Transcript`], [`InMemoryTranscript`] |
//! | Observe | [`KernelEvent`], [`EventBus`] |
//!
//! **Not in this crate:** session graph, fork/tombstone, Handout, product prefs, Rig/HTTP,
//! permission/approval policy engines (adapter-owned). `ToolApprovalRequired` is stream shape only.
//! Tree-agent mechanisms belong in `phi-ext-*`.
//!
//! **Naming:** use **SendQueue**, never "mailbox" (reserved for a future agent notify bus).
//!
//! **Generation commit:** stream batches use [`send_queue`] `EffectBatch` —
//! atomic response/result commits precede their observations; terminal
//! jobs persist only their status (see `send_queue::effect`).
//!
//! **Tool results:** outcome is [`ToolResultStatus`] only; `output` JSON is opaque to the kernel.
//!
//! **Prefix vs turn:** [`AgentPrefix`] is binding-owned; [`TurnRequest`] carries a snapshot.
//! Pump injects [`AgentPorts`] = [`AgentRuntime`] + [`TurnMaterials`] (prepare; typically
//! [`SourcesTurnMaterials`] over prefix + open-tool sources).

#![forbid(unsafe_code)]

/// Crate-private boilerplate for open string labels (see module docs).
#[macro_use]
mod string_newtype;

// Modules are private; crate root re-exports are the public surface.
mod agent;
mod content;
mod error;
mod events;
mod ids;
mod send_queue;
mod transcript;

pub use agent::{
    AgentEvent, AgentEventStream, AgentPorts, AgentPrefix, AgentPrefixSource, AgentRun,
    AgentRunLifecycle, AgentRuntime, EmptyAgentPrefix, FixedAgentPrefix, FixedToolCallSeal,
    ModelResponse, OneshotModel, OneshotRequest, OneshotText, PreambleSection,
    ProviderContinuation, ResponseUsageDrain, SkillDesc, SkillSlug, SourcesTurnMaterials,
    ToolArguments, ToolCallId, ToolCallSealPolicy, ToolCallSealSource, ToolName, ToolResultStatus,
    ToolSpec, TurnCancel, TurnItem, TurnMaterials, TurnRequest, Usage,
};
pub use content::{ContentPart, MessageContent, TailState};
pub use error::{KernelError, Result};
pub use events::{EventBus, KernelEvent};
pub use ids::{ImageId, JobId, MessageId, ModelResponseId, SessionId};
pub use send_queue::{GenerationJob, SendQueue, SessionDirectory};
pub use transcript::{
    GenerationCommit, GenerationRecord, GenerationStamp, GenerationStatus, InMemoryTranscript,
    RecordResult, Transcript, TranscriptRow, TranscriptSession, TruncateResult,
};

pub const KERNEL_NAME: &str = "phi-kernel";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod integration;
