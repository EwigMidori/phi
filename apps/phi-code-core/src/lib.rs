//! # phi-code-core
//!
//! Product runtime for phi-code (agent orchestration, session policy).
//!
//! Terminal UI lives in `phi-code-ui` — this crate must not grow TUI
//! view/layout/paint responsibilities.

#![forbid(unsafe_code)]

pub use phi_kernel::{ToolCallId, ToolName, ToolResultStatus, TurnItem};

pub const CORE_NAME: &str = "phi-code-core";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
