//! # phi-code-core
//!
//! Product runtime for phi-code (agent orchestration, LLM adapters).
//!
//! Terminal UI lives in `phi-code-ui` — this crate must not grow TUI
//! view/layout/paint responsibilities.

#![forbid(unsafe_code)]

pub mod llm;
pub mod turn_runner;

pub use llm::{ApiStyle, LlmConfig, LlmConfigError, OpenAiCompatRuntime};
pub use phi_kernel::{
    AgentEvent, AgentRuntime, JobId, SessionId, ToolCallId, ToolName, ToolResultStatus, TurnItem,
    TurnRequest,
};
pub use turn_runner::SessionTurnRunner;

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
