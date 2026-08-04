//! Delegation contract: request / result shapes and the [`Subagent`] /
//! [`SubagentSource`] ports.
//!
//! One subagent invocation is **tool-shaped**: it opens like a tool call
//! (`ToolCallId` + `ToolName` + opaque `input`) and closes like a tool result
//! ([`SubagentResult`] with an explicit [`ToolResultStatus`]). No stream and no
//! partial updates — a delegation is a single request/result round trip.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use phi_kernel::{SessionId, ToolCallId, ToolName, ToolResultStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How a delegation is materialized relative to the session graph.
///
/// Both variants exist in v0 because the request shape must be final before the
/// v1 runner can route on it; only `TreeChild` touches the graph. **No
/// [`Default`]** — every delegation states its disposition explicitly.
///
/// Relation to [`SubagentRequest::parent_session_id`]: under `TreeChild` that id
/// is the **derivation source** for a new child session node; under `Ephemeral`
/// there is **no session node**, and the id only feeds binding-material
/// preparation for the child generation (the ephemeral child's own session
/// identity is a v1 runner decision).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpawnMode {
    /// One-shot generation with **no graph impact** — no derived session node.
    Ephemeral,
    /// The subagent becomes a **derived session node** of the parent session
    /// (v1 wiring through [`crate::spawn::SessionSpawner`]).
    TreeChild,
}

/// Input to [`Subagent::run`] — the complete description of one delegation.
///
/// Field rationale ("why here"):
/// - `tool_call_id`: the parent turn's tool ledger correlates delegations by
///   kernel [`ToolCallId`]; the v1 runner must report the outcome against exactly
///   this call, so the id rides on the request.
/// - `tool_name`: the delegation's address — [`SubagentSource`] resolves the
///   [`Subagent`] by it, and it matches the parent's tool catalog name.
/// - `input`: opaque tool input (kernel discipline: `input` / `output` are
///   `serde_json::Value`, never interpreted by this crate).
/// - `parent_session_id`: origin session; semantics differ per [`SpawnMode`] —
///   see the field doc below (`TreeChild` derivation source vs `Ephemeral`
///   materials-only).
/// - `mode`: explicit per-call spawn disposition (`Ephemeral` vs `TreeChild`),
///   so the runner never has to guess how to materialize the subagent.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRequest {
    pub tool_call_id: ToolCallId,
    pub tool_name: ToolName,
    pub input: Value,
    /// The session the delegation originates from.
    ///
    /// Dual-mode semantics — depends on [`Self::mode`]:
    /// - `SpawnMode::TreeChild`: this id is the **derivation source**. v1 runs
    ///   it through [`crate::spawn::SessionSpawner::spawn_child`] to derive the
    ///   child session node.
    /// - `SpawnMode::Ephemeral`: there is **no session node**. The id exists
    ///   only so the v1 runner can prepare binding material for the child
    ///   generation (e.g. [`phi_kernel::AgentPrefixSource::prefix_for`]); the
    ///   ephemeral child generation's own `session_id` is an **open v1 runner
    ///   decision** — the parent's session id must never be reused as the child
    ///   generation id inside the parent's transcript.
    pub parent_session_id: SessionId,
    pub mode: SpawnMode,
}

/// The outcome of one delegation — the ext twin of
/// [`phi_kernel::AgentEvent::ToolResult`].
///
/// `status` is the **sole outcome authority**; `output` is opaque and must never
/// be sniffed for status (kernel discipline). **No [`Default`]** — every
/// delegation reports an explicit status. Correlation back to the originating
/// [`ToolCallId`] is the caller's job: each `run` is one request → one result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentResult {
    pub status: ToolResultStatus,
    pub output: Value,
}

/// A runnable subagent.
///
/// v0 defines the **face only** — this crate ships no runner. Products and
/// adapters implement this trait; the v1 runner (an `AgentRuntime` +
/// `TurnMaterials` composition) invokes it and closes the parent's tool call.
#[async_trait]
pub trait Subagent: Send + Sync {
    /// Execute one delegation and close it with a [`SubagentResult`].
    ///
    /// `request` is **borrowed**: after the `await` the v1 runner still needs
    /// `request.tool_call_id` to backfill the parent's tool ledger — an owning
    /// argument would force an early `clone`, and "extract the id or lose the
    /// correlation" is an interface-induced error class that borrowing removes
    /// at the type level. Implementations emit a fresh child-generation output,
    /// so borrowing costs nothing.
    async fn run(&self, request: &SubagentRequest) -> SubagentResult;
}

/// Resolves the [`Subagent`] bound to a tool name.
///
/// **Binding, not ACL** — mirrors [`phi_kernel::AgentPrefixSource`]. Permission
/// is product-side; this port only answers "which subagent handles this tool".
/// The default empty binding is [`NoSubagents`].
pub trait SubagentSource: Send + Sync {
    fn subagent_for(&self, tool_name: &ToolName) -> Option<Arc<dyn Subagent>>;
}

/// Empty binding: every tool name resolves to `None` (tests / default assembly).
#[derive(Clone, Debug, Default)]
pub struct NoSubagents;

impl SubagentSource for NoSubagents {
    fn subagent_for(&self, _tool_name: &ToolName) -> Option<Arc<dyn Subagent>> {
        None
    }
}

/// Fixed map-backed binding (`ToolName` → [`Subagent`]).
///
/// Mirrors [`phi_kernel::FixedAgentPrefix`]: products compose real subagents
/// here; tests inject doubles. Lookup only — resolution order is irrelevant.
#[derive(Clone)]
pub struct MapSubagents(pub HashMap<ToolName, Arc<dyn Subagent>>);

impl SubagentSource for MapSubagents {
    fn subagent_for(&self, tool_name: &ToolName) -> Option<Arc<dyn Subagent>> {
        self.0.get(tool_name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MapSubagents, NoSubagents, SpawnMode, Subagent, SubagentRequest, SubagentResult,
        SubagentSource,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use phi_kernel::{SessionId, ToolCallId, ToolName, ToolResultStatus};
    use serde_json::json;

    /// Test double: echoes the request input back as the delegation output.
    struct EchoSubagent;

    #[async_trait]
    impl Subagent for EchoSubagent {
        async fn run(&self, request: &SubagentRequest) -> SubagentResult {
            SubagentResult {
                status: ToolResultStatus::Ok,
                output: request.input.clone(),
            }
        }
    }

    /// Minimal fixture request exercising every field.
    fn request(mode: SpawnMode) -> SubagentRequest {
        SubagentRequest {
            tool_call_id: ToolCallId::new("tc-1"),
            tool_name: ToolName::new("research"),
            input: json!({"q": "graphs"}),
            parent_session_id: SessionId::generate(),
            mode,
        }
    }

    #[tokio::test]
    async fn subagent_run_closes_with_status_and_output() {
        let subagent = Arc::new(EchoSubagent) as Arc<dyn Subagent>;
        let req = request(SpawnMode::Ephemeral);
        let result = subagent.run(&req).await;
        assert_eq!(result.status, ToolResultStatus::Ok);
        assert_eq!(result.output, json!({"q": "graphs"}));
        // The borrowed request survives the await — correlation is never lost.
        assert_eq!(req.tool_call_id, ToolCallId::new("tc-1"));
    }

    #[test]
    fn subagent_source_resolves_bound_tool_and_misses_unknown() {
        let source = MapSubagents(HashMap::from([(
            ToolName::new("research"),
            Arc::new(EchoSubagent) as Arc<dyn Subagent>,
        )]));
        assert!(source.subagent_for(&ToolName::new("research")).is_some());
        assert!(source.subagent_for(&ToolName::new("unknown")).is_none());
        assert!(NoSubagents.subagent_for(&ToolName::new("any")).is_none());
    }

    #[test]
    fn spawn_mode_has_both_variants_and_serde_shape() {
        assert_ne!(SpawnMode::Ephemeral, SpawnMode::TreeChild);
        assert_eq!(
            serde_json::to_string(&SpawnMode::Ephemeral).unwrap(),
            "\"ephemeral\""
        );
        assert_eq!(
            serde_json::to_string(&SpawnMode::TreeChild).unwrap(),
            "\"treeChild\""
        );
    }

    #[test]
    fn subagent_result_status_is_the_authority() {
        let result = SubagentResult {
            status: ToolResultStatus::Denied,
            output: json!({"reason": "no thanks"}),
        };
        // Read from the field — never derived from output keys.
        assert_eq!(result.status, ToolResultStatus::Denied);
        assert_eq!(result.output["reason"], "no thanks");
    }
}
