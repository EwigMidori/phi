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
//! | Spawn | [`SessionSpawner`] |
//!
//! **Tool-shaped delegation:** from the parent turn's view one subagent invocation
//! is exactly one tool call — [`SubagentRequest`] carries the kernel [`ToolCallId`]
//! / [`ToolName`] and an opaque `input` — and closes with one [`SubagentResult`]
//! whose [`ToolResultStatus`] is the sole outcome authority (`output` is never
//! sniffed). No stream, no partials.
//!
//! **Dispatch is caller-first, resolver-last:** the initiator decides whether a
//! call is a delegation and which subagent executes it when one is known;
//! [`SubagentResolver`] is consulted only for a confirmed delegation with no
//! explicitly chosen subagent, and always answers.
//!
//! **Not in this crate:** the runner (v1), ACL / permission engines (product),
//! session-graph derive (`phi-ext-tree-agent`).
//!
//! **Spawn caps: deliberately not in v0.** Budget is **product policy**, not
//! mechanism — this crate prescribes no budget shape. The v1 runner will
//! consult an injected `SpawnPolicy` port (mechanism asks, product answers —
//! same posture as the tree-agent `ClosePolicy`); concrete cap structs demote
//! to a provided default impl in v1, not to a v0 type.
//!
//! **Naming:** spawn / delegate / result vocabulary — never "mailbox" (reserved
//! for a future agent async-notification bus).

#![forbid(unsafe_code)]

pub mod spawn;
pub mod subagent;

pub use spawn::SessionSpawner;
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
        // Resolution is unconditional for a bound tool — no `None` channel.
        let subagent = source.resolve(&DelegationContext::new(
            inner.tool_name.clone(),
            inner.input.clone(),
            inner.parent_session_id.clone(),
        ));
        let result = subagent.run(&req).await;
        assert_eq!(result.status, ToolResultStatus::Ok);
        assert_eq!(result.output, json!({"q": "graphs"}));
        // The borrowed request survives the await — correlation is never lost.
        assert_eq!(req.tool_call_id(), &ToolCallId::new("tc-1"));
    }

    #[test]
    #[should_panic(expected = "not bound")]
    fn unbound_tool_is_a_configuration_error() {
        let source = MapSubagents(HashMap::from([(
            ToolName::new("research"),
            Arc::new(EchoSubagent) as Arc<dyn Subagent>,
        )]));
        let _ = source.resolve(&DelegationContext::new(
            ToolName::new("missing"),
            json!({}),
            SessionId::generate(),
        ));
    }
}
