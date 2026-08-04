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

/// How a delegation is materialized — the **outer discriminant** of the request.
///
/// The session-id field lives on the variant that owns its semantics, so a
/// field's meaning can never depend on another field:
/// - `TreeChild` carries `parent_session_id` — a **real parent** (the derivation
///   source for a new child session node).
/// - `Ephemeral` carries `origin_session_id` — **not a parent** (there is no
///   session node); it only feeds binding-material preparation for the child
///   generation. The ephemeral child's own session identity is a v1 runner
///   decision.
///
/// Common-field rationale ("why here"):
/// - `tool_call_id`: the parent turn's tool ledger correlates delegations by
///   kernel [`ToolCallId`]; the v1 runner must report the outcome against
///   exactly this call, so the id rides on the request.
/// - `tool_name`: the delegation's **address** — [`SubagentSource`] resolves the
///   [`Subagent`] by it, and it matches the parent's tool catalog name.
/// - `input`: opaque tool input (kernel discipline: `input` / `output` are
///   `serde_json::Value`, never interpreted by this crate).
///
/// **No `Default`** — every delegation states its disposition as the variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum SubagentRequest {
    /// The subagent becomes a **derived session node** of the parent session
    /// (v1 wiring through [`crate::spawn::SessionSpawner`]).
    TreeChild(TreeChildRequest),
    /// One-shot generation with **no graph impact** — no derived session node.
    Ephemeral(EphemeralRequest),
}

impl SubagentRequest {
    /// The parent turn's tool-call correlation key — common to both variants.
    #[must_use]
    pub fn tool_call_id(&self) -> &ToolCallId {
        match self {
            Self::TreeChild(r) => &r.tool_call_id,
            Self::Ephemeral(r) => &r.tool_call_id,
        }
    }

    /// The delegation's address — common to both variants.
    #[must_use]
    pub fn tool_name(&self) -> &ToolName {
        match self {
            Self::TreeChild(r) => &r.tool_name,
            Self::Ephemeral(r) => &r.tool_name,
        }
    }

    /// Opaque tool input — common to both variants.
    #[must_use]
    pub fn input(&self) -> &Value {
        match self {
            Self::TreeChild(r) => &r.input,
            Self::Ephemeral(r) => &r.input,
        }
    }
}

/// The `TreeChild` message: a delegation that derives a child session node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeChildRequest {
    pub tool_call_id: ToolCallId,
    pub tool_name: ToolName,
    pub input: Value,
    /// The **derivation source** — the real parent session. v1 runs it through
    /// [`crate::spawn::SessionSpawner::spawn_child`] to derive the child node.
    pub parent_session_id: SessionId,
}

/// The `Ephemeral` message: a one-shot delegation with no session node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EphemeralRequest {
    pub tool_call_id: ToolCallId,
    pub tool_name: ToolName,
    pub input: Value,
    /// Origin session used only to prepare binding material for the child
    /// generation (e.g. [`phi_kernel::AgentPrefixSource::prefix_for`]). **Not a
    /// parent** — there is no session node; the ephemeral child generation's
    /// own `session_id` is an open v1 runner decision and must never reuse this
    /// id inside the parent's transcript.
    pub origin_session_id: SessionId,
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
    /// `request.tool_call_id()` to backfill the parent's tool ledger — an
    /// owning argument would force an early `clone`, and "extract the id or
    /// lose the correlation" is an interface-induced error class that borrowing
    /// removes at the type level. Implementations emit a fresh child-generation
    /// output, so borrowing costs nothing.
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
        EphemeralRequest, MapSubagents, NoSubagents, Subagent, SubagentRequest, SubagentResult,
        SubagentSource, TreeChildRequest,
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
                output: request.input().clone(),
            }
        }
    }

    /// Minimal `TreeChild` fixture request exercising the derived-node path.
    fn tree_child_request() -> SubagentRequest {
        SubagentRequest::TreeChild(TreeChildRequest {
            tool_call_id: ToolCallId::new("tc-1"),
            tool_name: ToolName::new("research"),
            input: json!({"q": "graphs"}),
            parent_session_id: SessionId::generate(),
        })
    }

    /// Minimal `Ephemeral` fixture request exercising the no-node path.
    fn ephemeral_request() -> SubagentRequest {
        SubagentRequest::Ephemeral(EphemeralRequest {
            tool_call_id: ToolCallId::new("tc-1"),
            tool_name: ToolName::new("research"),
            input: json!({"q": "graphs"}),
            origin_session_id: SessionId::generate(),
        })
    }

    #[tokio::test]
    async fn subagent_run_closes_with_status_and_output() {
        let subagent = Arc::new(EchoSubagent) as Arc<dyn Subagent>;
        let req = ephemeral_request();
        let result = subagent.run(&req).await;
        assert_eq!(result.status, ToolResultStatus::Ok);
        assert_eq!(result.output, json!({"q": "graphs"}));
        // The borrowed request survives the await — correlation is never lost.
        assert_eq!(req.tool_call_id(), &ToolCallId::new("tc-1"));
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
    fn subagent_request_tree_child_serde_shape() {
        let req = tree_child_request();
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["mode"], "treeChild");
        assert!(value.get("parentSessionId").is_some());
        let roundtrip: SubagentRequest = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, req);
    }

    #[test]
    fn subagent_request_ephemeral_serde_shape() {
        let req = ephemeral_request();
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["mode"], "ephemeral");
        assert!(value.get("originSessionId").is_some());
        // Core remediation: an Ephemeral message must never carry a parent.
        assert!(value.get("parentSessionId").is_none());
        let roundtrip: SubagentRequest = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, req);
    }

    #[test]
    fn common_fields_accessible_across_variants() {
        let tree = tree_child_request();
        let ephemeral = ephemeral_request();
        assert_eq!(tree.tool_call_id(), ephemeral.tool_call_id());
        assert_eq!(tree.tool_name(), ephemeral.tool_name());
        assert_eq!(tree.input(), ephemeral.input());
        assert_eq!(tree.tool_call_id(), &ToolCallId::new("tc-1"));
        assert_eq!(tree.tool_name(), &ToolName::new("research"));
        assert_eq!(tree.input(), &json!({"q": "graphs"}));
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
