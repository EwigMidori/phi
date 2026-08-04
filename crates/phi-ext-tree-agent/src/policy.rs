//! Injected close strategy (mechanism vs strategy).
//!
//! The tree owns topology; the *decision* of what closing a node does is a
//! strategy injected as [`ClosePolicy`] at
//! [`crate::tree::SessionTree::open`]. Policy types have **no [`Default`]** —
//! they are explicitly constructed and injected, matching the kernel's
//! no-default stance.

use crate::model::NodeState;

/// What should happen to a closed node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseDisposition {
    /// Remove the node and its incident edges. The tree honors this for **leaf
    /// nodes only** and refuses it otherwise (removing a non-leaf would orphan
    /// its children — the tree enforces its invariants against any policy).
    HardRemove,
    /// Mark the node Tombstoned and keep its edges (subtree stays connected).
    Tombstone,
}

/// Facts the tree hands a close policy so it can decide.
///
/// Leaf-ness is resolved by the tree from live topology and passed in — a
/// policy must not query topology itself. This keeps policies stateless, makes
/// the tree the single topology authority, and means a policy never needs to
/// call back into the aggregate (which would re-enter the tree lock and
/// deadlock).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeCloseContext {
    /// The node's state at close time.
    pub state: NodeState,
    /// True when the node has no children at close time.
    pub is_leaf: bool,
}

impl NodeCloseContext {
    /// A close-time fact set.
    #[must_use]
    pub fn new(state: NodeState, is_leaf: bool) -> Self {
        Self { state, is_leaf }
    }
}

/// Decides what closing a node should do.
///
/// This is the close-rule extension point:
/// [`crate::tree::SessionTree::close`] calls it **while the tree lock is
/// held**, so implementations must be stateless and cheap (`Send + Sync`), and
/// **must not call back into any [`crate::tree::SessionTree`] method** —
/// re-entering the same lock would deadlock. Every fact a decision needs is
/// passed in [`NodeCloseContext`].
pub trait ClosePolicy: Send + Sync {
    /// Map a node's close-time facts to a disposition.
    fn disposition(&self, ctx: &NodeCloseContext) -> CloseDisposition;
}

/// Standard close rule (the Daan product rule):
///
/// | state \ leaf | leaf | non-leaf |
/// |--------------|------|----------|
/// | Live         | hard remove | tombstone |
/// | Tombstoned   | hard remove | tombstone (already soft-deleted → the tree no-ops) |
///
/// **No [`Default`]** — construct and inject explicitly at
/// [`crate::tree::SessionTree::open`].
#[derive(Clone, Copy, Debug)]
pub struct StandardClosePolicy;

impl ClosePolicy for StandardClosePolicy {
    fn disposition(&self, ctx: &NodeCloseContext) -> CloseDisposition {
        match (ctx.state, ctx.is_leaf) {
            (NodeState::Live | NodeState::Tombstoned, true) => CloseDisposition::HardRemove,
            (NodeState::Live | NodeState::Tombstoned, false) => CloseDisposition::Tombstone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_close_policy_follows_the_product_rule() {
        let policy = StandardClosePolicy;
        let ctx = NodeCloseContext::new;
        // Live leaf → hard remove; live non-leaf → tombstone.
        assert_eq!(
            policy.disposition(&ctx(NodeState::Live, true)),
            CloseDisposition::HardRemove
        );
        assert_eq!(
            policy.disposition(&ctx(NodeState::Live, false)),
            CloseDisposition::Tombstone
        );
        // Tombstoned leaf → hard remove; tombstoned non-leaf → tombstone (tree no-ops it).
        assert_eq!(
            policy.disposition(&ctx(NodeState::Tombstoned, true)),
            CloseDisposition::HardRemove
        );
        assert_eq!(
            policy.disposition(&ctx(NodeState::Tombstoned, false)),
            CloseDisposition::Tombstone
        );
    }
}
