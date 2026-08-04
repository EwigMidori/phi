//! Spawn port: the `TreeChild` derivation port.
//!
//! v0 ships only the derive port. **Spawn caps are deliberately absent**: budget
//! is product policy, not mechanism — the v1 runner will consult an injected
//! `SpawnPolicy` port (mechanism asks, product answers; no shape prescribed
//! here). The derive-side implementation of [`SessionSpawner`] belongs to
//! `phi-ext-tree-agent` or a product.

use async_trait::async_trait;
use phi_kernel::SessionId;

/// Creates a **derived child session node** for [`crate::subagent::SubagentRequest::TreeChild`] delegations.
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
/// shape already carries a `TreeChild` variant, so the v1 runner needs a place
/// to wire the derive.
#[async_trait]
pub trait SessionSpawner: Send + Sync {
    /// Derive a child session from `parent_session_id` and return the new id.
    ///
    /// The port carries only facts the receiver needs — `tool_name` has **no
    /// consumer** here: a tree-agent `SessionTree::derive(parent, WithHistory)`
    /// stores no label, and the delegation's catalog name is already held by the
    /// runner via [`crate::subagent::SubagentRequest::tool_name`]. The derivation
    /// is **with-history** (`WithHistory`) per the trait doc; exact semantics
    /// are the wiring side's, not this port's. Errors follow the kernel
    /// `AgentRuntime::run` convention (`String`): spawn failure is infra failure
    /// and becomes a [`phi_kernel::ToolResultStatus::Error`] result in the v1
    /// runner.
    async fn spawn_child(&self, parent_session_id: &SessionId) -> Result<SessionId, String>;
}

#[cfg(test)]
mod tests {
    use super::SessionSpawner;
    use std::sync::Arc;

    use async_trait::async_trait;
    use phi_kernel::SessionId;

    /// Test double: returns a fixed derived child session id.
    struct FixedChildSpawner {
        child: Arc<SessionId>,
    }

    #[async_trait]
    impl SessionSpawner for FixedChildSpawner {
        async fn spawn_child(&self, _parent_session_id: &SessionId) -> Result<SessionId, String> {
            Ok((*self.child).clone())
        }
    }

    #[tokio::test]
    async fn session_spawner_port_returns_derived_session() {
        let parent = SessionId::generate();
        let child = SessionId::generate();
        let spawner = FixedChildSpawner {
            child: Arc::new(child.clone()),
        };
        let child_session = spawner.spawn_child(&parent).await.unwrap();
        assert_eq!(child_session, child);
        assert_ne!(child_session, parent);
    }
}
