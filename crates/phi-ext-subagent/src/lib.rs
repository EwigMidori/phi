//! # phi-ext-subagent
//!
//! Parent/child agent **delegation protocol** for [phi](https://github.com/ewigmidori/phi):
//! **tool-shaped** subagent spawn. v0 ships the **interface face only** — the
//! types and ports below are final; the runner (an `AgentRuntime` +
//! `TurnMaterials` composition) lands in v1.
//!
//! | Area | Surface |
//! |------|---------|
//! | Delegation | [`SubagentRequest`], [`SubagentResult`], [`Subagent`] |
//! | Binding | [`SubagentSource`], [`NoSubagents`], [`MapSubagents`] |
//! | Spawn | [`SpawnMode`], [`SessionSpawner`], [`SpawnBudget`] |
//!
//! **Tool-shaped delegation:** from the parent turn's view one subagent invocation
//! is exactly one tool call — [`SubagentRequest`] carries the kernel [`ToolCallId`]
//! / [`ToolName`] and an opaque `input` — and closes with one [`SubagentResult`]
//! whose [`ToolResultStatus`] is the sole outcome authority (`output` is never
//! sniffed). No stream, no partials.
//!
//! **Not in this crate:** the runner (v1), ACL / permission engines (product),
//! session-graph derive (`phi-ext-tree-agent`).
//!
//! **Naming:** spawn / delegate / result vocabulary — never "mailbox" (reserved
//! for a future agent async-notification bus).

#![forbid(unsafe_code)]

pub mod spawn;
pub mod subagent;

pub use spawn::{SessionSpawner, SpawnBudget};
pub use subagent::{
    MapSubagents, NoSubagents, SpawnMode, Subagent, SubagentRequest, SubagentResult, SubagentSource,
};

#[cfg(test)]
mod integration {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use phi_kernel::{SessionId, ToolCallId, ToolName, ToolResultStatus};
    use serde_json::json;

    use super::{
        MapSubagents, SpawnMode, Subagent, SubagentRequest, SubagentResult, SubagentSource,
    };

    /// Test double: echoes the request input back as the delegation output.
    struct EchoSubagent;

    #[async_trait]
    impl Subagent for EchoSubagent {
        async fn run(&self, request: SubagentRequest) -> SubagentResult {
            SubagentResult {
                status: ToolResultStatus::Ok,
                output: request.input,
            }
        }
    }

    #[tokio::test]
    async fn delegated_tool_call_closes_with_tool_result() {
        let source = MapSubagents(HashMap::from([(
            ToolName::new("research"),
            Arc::new(EchoSubagent) as Arc<dyn Subagent>,
        )]));
        let subagent = source
            .subagent_for(&ToolName::new("research"))
            .expect("bound tool resolves");
        let result = subagent
            .run(SubagentRequest {
                tool_call_id: ToolCallId::new("tc-1"),
                tool_name: ToolName::new("research"),
                input: json!({"q": "graphs"}),
                parent_session_id: SessionId::generate(),
                mode: SpawnMode::TreeChild,
            })
            .await;
        assert_eq!(result.status, ToolResultStatus::Ok);
        assert_eq!(result.output, json!({"q": "graphs"}));
        assert!(source.subagent_for(&ToolName::new("missing")).is_none());
    }
}
