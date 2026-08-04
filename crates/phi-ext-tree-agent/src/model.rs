//! Shared tree model types (nodes, edges) used by the aggregate, the events,
//! and the snapshots.
//!
//! **Tree, not graph:** every node has exactly one parent (the root has none);
//! multi-parent, DAG, and merge are out of scope for this crate.

use serde::{Deserialize, Serialize};

use phi_kernel::SessionId;

/// Lifecycle state of a tree node.
///
/// Mirrors the product's (Daan) `SoftDeletedSession` placeholder semantics: a
/// `Tombstoned` node stays in the topology (its subtree stays connected) but
/// can no longer be derived from or focused.
///
/// **No [`Default`]** — a node must be created in an explicit state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeState {
    /// Open for derivation and focus.
    Live,
    /// Soft-deleted: kept in the topology, not derivable, not focusable.
    Tombstoned,
}

/// How the parent → child edge was created.
///
/// **Explicit parameter, no [`Default`]** — the caller of
/// [`crate::tree::SessionTree::derive`] must pick one; there is no implicit
/// "history follows" fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EdgeKind {
    /// The child continues with the parent's history (e.g. a branch that keeps
    /// the whole dialogue).
    WithHistory,
    /// The child starts without the parent's history (e.g. a reset branch).
    WithoutHistory,
}

/// A node's incoming edge: its parent and the edge kind.
///
/// In a single-parent tree the parent and the edge kind are **one fact**, not
/// two independent `Option`s — the aggregate stores them together as
/// `Option<ParentEdge>` on the child's record (a root has `None`). This type is
/// also what [`crate::tree::SessionTree::incoming_edge`] returns, so the edge
/// kind is queryable at all (a bare-parent accessor would drop it).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentEdge {
    /// The parent node.
    pub parent: SessionId,
    /// How the parent → child edge was created.
    pub kind: EdgeKind,
}

impl ParentEdge {
    /// A parent edge.
    #[must_use]
    pub fn new(parent: SessionId, kind: EdgeKind) -> Self {
        Self { parent, kind }
    }
}

/// A directed parent → child edge with **both endpoints explicit**.
///
/// This is the serializable snapshot shape: a flat edge list has no "child
/// side" to hang the parent on, so it carries both ids. Inside the aggregate
/// the same fact lives on the child as [`ParentEdge`] (parent + kind; the
/// child id is implicit in the map key). [`crate::tree::SessionTree::snapshot`]
/// derives these from the per-child [`ParentEdge`]s.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEdge {
    /// The parent node (exists and was Live at derivation time).
    pub parent: SessionId,
    /// The child node.
    pub child: SessionId,
    /// Whether the child continues with the parent's history.
    pub kind: EdgeKind,
}

#[cfg(test)]
mod tests {
    use super::{EdgeKind, NodeState, ParentEdge};
    use serde_json::json;

    #[test]
    fn node_state_serde_is_camel_case() {
        assert_eq!(
            serde_json::to_value(NodeState::Live).unwrap(),
            json!("live")
        );
        assert_eq!(
            serde_json::to_value(NodeState::Tombstoned).unwrap(),
            json!("tombstoned")
        );
    }

    #[test]
    fn edge_kind_serde_is_camel_case() {
        assert_eq!(
            serde_json::to_value(EdgeKind::WithHistory).unwrap(),
            json!("withHistory")
        );
        assert_eq!(
            serde_json::to_value(EdgeKind::WithoutHistory).unwrap(),
            json!("withoutHistory")
        );
    }

    #[test]
    fn parent_edge_serde_is_camel_case() {
        let edge = ParentEdge::new("p".parse().unwrap(), EdgeKind::WithHistory);
        let value = serde_json::to_value(&edge).unwrap();
        assert_eq!(value["parent"], json!("p"));
        assert_eq!(value["kind"], json!("withHistory"));
        let round: ParentEdge = serde_json::from_value(value).unwrap();
        assert_eq!(round, edge);
    }
}
