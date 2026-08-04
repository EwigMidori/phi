//! Spawn ports: the `TreeChild` derivation port and the spawn budget.
//!
//! v0 **declares, not enforces** — depth / per-turn budget enforcement belongs to
//! the v1 runner; the derive-side implementation of [`SessionSpawner`] belongs to
//! `phi-ext-tree-agent` or a product.

use async_trait::async_trait;
use phi_kernel::{SessionId, ToolName};
use serde::{Deserialize, Serialize};

/// Creates a **derived child session node** for `SpawnMode::TreeChild` delegations.
///
/// A `TreeChild` delegation is a **with-history delegation** (context-carrying):
/// the child is derived as `WithHistory` — it inherits the parent's context
/// rather than starting blank. That derive choice belongs to the **wiring
/// side**: v1 implements this port in `phi-ext-tree-agent` or a product, and the
/// `WithHistory` semantics live there, deliberately **outside this port** — this
/// crate therefore keeps depending on `phi-kernel` only (no reverse dependency
/// on `phi-ext-tree-agent`).
///
/// The port exists in v0 even though nothing implements it yet: the request
/// shape already carries `TreeChild`, so the v1 runner needs a place to wire the
/// derive.
#[async_trait]
pub trait SessionSpawner: Send + Sync {
    /// Derive a child session from `parent_session_id` and return the new id.
    ///
    /// `tool_name` labels the derivation (the child's reason for existing).
    /// The derivation is **with-history** (`WithHistory`) per the trait doc;
    /// exact semantics are the wiring side's, not this port's. Errors follow the
    /// kernel `AgentRuntime::run` convention (`String`): spawn failure is infra
    /// failure and becomes a [`phi_kernel::ToolResultStatus::Error`] result in
    /// the v1 runner.
    async fn spawn_child(
        &self,
        parent_session_id: &SessionId,
        tool_name: &ToolName,
    ) -> Result<SessionId, String>;
}

/// Caps on subagent spawning, enforced by the **v1 runner**.
///
/// **No [`Default`]** — a budget is explicit policy, and the repo rule is that
/// policy has no implicit default. Construct via struct literal so every call
/// site states both caps; v1 enforces them before each spawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnBudget {
    /// Maximum delegation nesting depth (`0` = no subagents at all).
    pub max_depth: u32,
    /// Maximum spawns per parent turn.
    pub max_per_turn: u32,
}

#[cfg(test)]
mod tests {
    use super::{SessionSpawner, SpawnBudget};
    use std::sync::Arc;

    use async_trait::async_trait;
    use phi_kernel::{SessionId, ToolName};

    /// Test double: returns a fixed derived child session id.
    struct FixedChildSpawner {
        child: Arc<SessionId>,
    }

    #[async_trait]
    impl SessionSpawner for FixedChildSpawner {
        async fn spawn_child(
            &self,
            _parent_session_id: &SessionId,
            _tool_name: &ToolName,
        ) -> Result<SessionId, String> {
            Ok((*self.child).clone())
        }
    }

    #[test]
    fn spawn_budget_is_explicit_pair_of_caps() {
        let budget = SpawnBudget {
            max_depth: 2,
            max_per_turn: 4,
        };
        assert_eq!(budget.max_depth, 2);
        assert_eq!(budget.max_per_turn, 4);
        // Copy: the budget travels with the request without surprises.
        let copy = budget;
        assert_eq!(copy.max_per_turn, budget.max_per_turn);
    }

    #[tokio::test]
    async fn session_spawner_port_returns_derived_session() {
        let parent = SessionId::generate();
        let child = SessionId::generate();
        let spawner = FixedChildSpawner {
            child: Arc::new(child.clone()),
        };
        let child_session = spawner
            .spawn_child(&parent, &ToolName::new("research"))
            .await
            .unwrap();
        assert_eq!(child_session, child);
        assert_ne!(child_session, parent);
    }
}
