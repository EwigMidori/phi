//! # phi-code-core
//!
//! Product runtime for phi-code: session host over kernel generation.
//!
//! LLM **wire** adapters live in `phi-ext-llm`. Terminal UI lives in
//! `phi-code-ui`.

#![forbid(unsafe_code)]

mod session;

// Crate root is the public surface; modules stay private.
pub use phi_kernel::{
    AgentEvent, AgentRuntime, InMemoryTranscript, KernelEvent, SessionId, Transcript, TurnItem,
    Usage,
};
pub use session::{PollBatch, SessionHost};

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
