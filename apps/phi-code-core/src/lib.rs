//! # phi-code-core
//!
//! Product runtime for phi-code: [`SessionHost`] + [`TurnDriver`].
//!
//! LLM **wire** adapters live in `phi-ext-llm`. Terminal UI lives in
//! `phi-code-ui`. Process env loading is host-owned (CLI).

#![forbid(unsafe_code)]

mod session;
mod turn_driver;

// Crate root is the public surface; modules stay private.
pub use phi_ext_llm::{ApiStyle, LlmConfig};
pub use phi_kernel::{
    AgentEvent, AgentRuntime, InMemoryTranscript, KernelEvent, SessionId, Transcript, TurnItem,
    Usage,
};
pub use session::{PollBatch, SessionHost};
pub use turn_driver::{ChannelInfo, SubmitOutcome, TickResult, TurnDriver, UsageInfo};

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
