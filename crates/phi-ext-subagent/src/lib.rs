//! # phi-ext-subagent
//!
//! Parent/child agent **delegation protocol** for [phi](https://github.com/ewigmidori/phi):
//! **tool-shaped** subagent spawn. v0 ships the **interface face only** — the
//! types and ports below are final, converged through two review rounds; the
//! runner (an `AgentRuntime` + `TurnMaterials` composition) lands in v1.
//!
//! | Area | Surface |
//! |------|---------|
//! | Delegation | [`SubagentRequest`] (`TreeChild` / `Ephemeral`), [`SubagentResult`], [`Subagent`] |
//! | Dispatch | [`SubagentResolver`], [`NoSubagents`], [`MapSubagents`] |
//! | Spawn | [`SessionSpawner`], [`SpawnBudget`] |
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
//! **Spawn budget:** [`SpawnBudget`] is the **v1 runner's composition config**
//! (constructor-injected, same posture as kernel [`phi_kernel::AgentPorts`]) —
//! not a per-delegation field on [`SubagentRequest`]; v0 builds no source port
//! for it.
//!
//! **Naming:** spawn / delegate / result vocabulary — never "mailbox" (reserved
//! for a future agent async-notification bus).

#![forbid(unsafe_code)]

pub mod spawn;
pub mod subagent;

pub use spawn::{SessionSpawner, SpawnBudget};
pub use subagent::{
    DelegationContext, EphemeralRequest, MapSubagents, NoSubagents, Subagent, SubagentRequest,
    SubagentResolver, SubagentResult, TreeChildRequest,
};

#[cfg(test)]
mod integration {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use phi_kernel::{SessionId, ToolCallId, ToolName, ToolResultStatus};
    use serde_json::json;

    use super::{
        DelegationContext, MapSubagents, Subagent, SubagentRequest, SubagentResolver,
        SubagentResult, TreeChildRequest,
    };

    /// Test double: echoes the request input back as the delegation output.
    struct EchoSubagent;

    #[async_trait]
    impl Subagent for EchoSubagent {
        async fn run(&self, request: &SubagentRequest) -> SubagentResult {
            SubagentResult {
                status: ToolResultStatus::Ok,
                output: request.input().clone(),
            }
        }
    }

    #[tokio::test]
    async fn delegated_tool_call_closes_with_tool_result() {
        let source = MapSubagents(HashMap::from([(
            ToolName::new("research"),
            Arc::new(EchoSubagent) as Arc<dyn Subagent>,
        )]));
        let req = SubagentRequest::TreeChild(TreeChildRequest {
            tool_call_id: ToolCallId::new("tc-1"),
            tool_name: ToolName::new("research"),
            input: json!({"q": "graphs"}),
            parent_session_id: SessionId::generate(),
        });
        let SubagentRequest::TreeChild(inner) = &req else {
            unreachable!("fixture is TreeChild")
        };
        let subagent = source
            .resolve(&DelegationContext::new(
                inner.tool_name.clone(),
                inner.input.clone(),
                inner.parent_session_id.clone(),
            ))
            .expect("bound tool resolves");
        let result = subagent.run(&req).await;
        assert_eq!(result.status, ToolResultStatus::Ok);
        assert_eq!(result.output, json!({"q": "graphs"}));
        // The borrowed request survives the await — correlation is never lost.
        assert_eq!(req.tool_call_id(), &ToolCallId::new("tc-1"));
        assert!(
            source
                .resolve(&DelegationContext::new(
                    ToolName::new("missing"),
                    json!({}),
                    inner.parent_session_id.clone(),
                ))
                .is_none()
        );
    }
}
