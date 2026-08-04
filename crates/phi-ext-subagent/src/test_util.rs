//! Shared test doubles for this crate (`#[cfg(test)]` only).

use async_trait::async_trait;
use phi_kernel::ToolResultStatus;

use crate::subagent::{Subagent, SubagentRequest, SubagentResult};

/// Test double: echoes the request input back as the delegation output.
pub struct EchoSubagent;

#[async_trait]
impl Subagent for EchoSubagent {
    async fn run(&self, request: &SubagentRequest) -> SubagentResult {
        SubagentResult {
            status: ToolResultStatus::Ok,
            output: request.input().clone(),
        }
    }
}
