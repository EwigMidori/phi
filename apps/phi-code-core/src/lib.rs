//! # phi-code-core
//!
//! Product runtime for phi-code (turn orchestration).
//!
//! LLM **wire** adapters live in `phi-ext-llm`. Terminal UI lives in
//! `phi-code-ui`.

#![forbid(unsafe_code)]

mod turn_runner;

// Crate root is the public surface; modules stay private.
// Re-export only kernel types that appear on this crate's public API.
pub use phi_kernel::{AgentEvent, AgentRuntime, SessionId, TurnItem};
pub use turn_runner::SessionTurnRunner;

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
