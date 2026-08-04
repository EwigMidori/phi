//! Crate-local errors for subagent resolution.

use phi_kernel::ToolName;
use thiserror::Error;

/// Errors from [`crate::subagent::SubagentResolver`].
///
/// This crate keeps its own error type instead of reusing the kernel's
/// [`phi_kernel::KernelError`]: resolution is a dispatch contract, not a
/// kernel queue concern. The shape mirrors kernel / tree-agent conventions
/// (typed variants, not-formatted messages).
///
/// Every variant is a **caller-side configuration error**: the resolver is
/// consulted only for a confirmed delegation with no explicitly chosen
/// subagent, so failing to produce one means the caller asked for something
/// that is not configured — never a normal outcome.
#[derive(Debug, Error)]
pub enum ResolutionError {
    /// The resolver was consulted for a tool name with no bound subagent
    /// (a [`crate::subagent::MapSubagents`] miss).
    #[error("no subagent bound for tool: {0}")]
    UnboundTool(ToolName),

    /// The resolver was consulted for a confirmed delegation but could not
    /// determine a target (e.g. an unknown name in the input).
    #[error("cannot resolve a subagent for this delegation: {0}")]
    Unresolved(String),
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, ResolutionError>;
