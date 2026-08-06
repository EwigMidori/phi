//! # phi-code-core
//!
//! Product runtime for phi-code (agent orchestration, LLM adapters).
//!
//! Terminal UI lives in `phi-code-ui` — this crate must not grow TUI
//! view/layout/paint responsibilities.

#![forbid(unsafe_code)]

mod llm;
mod turn_runner;

// Crate root is the public surface; modules stay private.
// Re-export only kernel types that appear on this crate's public API.
pub use llm::{ApiStyle, LlmConfig, LlmConfigError, OpenAiCompatRuntime};
pub use phi_kernel::{AgentEvent, AgentRuntime, SessionId, TurnItem};
pub use turn_runner::SessionTurnRunner;

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
