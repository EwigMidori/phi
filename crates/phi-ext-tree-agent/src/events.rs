//! Tree observation events, projected by the caller.
//!
//! Mutation methods return [`Vec<TreeEvent>`]; there is **no built-in event
//! bus** — keeping the aggregate testable and free of bus dependencies.
//! Serialization follows kernel style (`#[serde(tag = "type",
//! rename_all = "camelCase")]`).

use serde::{Deserialize, Serialize};

use phi_kernel::SessionId;

use crate::model::EdgeKind;

/// A tree mutation, in emission order.
///
/// [`crate::tree::SessionTree::derive`] emits `[NodeCreated, EdgeCreated]`
/// (node first, so a projection can require the node before linking the edge).
/// [`crate::tree::SessionTree::remove`] and
/// [`crate::tree::SessionTree::remove_subtree`] emit `NodeRemoved` events
/// (remove_subtree: one per removed node, **top-down**). [`crate::tree::SessionTree::reparent`]
/// emits `[EdgeReparented]`.
///
/// Field rationale (minimal projection set): a projection must be able to (a)
/// create a node, (b) link it to a parent with its edge kind, (c) drop a
/// removed node *and* its parent edge, and (d) move an existing node to a new
/// parent with a (possibly new) edge kind. (c) is why `NodeRemoved` carries
/// `parent` — without it there would be no way to drop the edge (there is
/// deliberately no `EdgeRemoved` event).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TreeEvent {
    /// A new node appeared (from `add_root` or `derive`).
    #[serde(rename_all = "camelCase")]
    NodeCreated {
        /// The new node.
        session_id: SessionId,
    },
    /// A parent → child edge appeared (from `derive`).
    #[serde(rename_all = "camelCase")]
    EdgeCreated {
        /// The existing parent node.
        parent: SessionId,
        /// The new child node.
        child: SessionId,
        /// The edge kind chosen at derivation.
        kind: EdgeKind,
    },
    /// A node was removed together with its parent edge (from `remove`, or
    /// from `remove_subtree` — one event per removed node).
    #[serde(rename_all = "camelCase")]
    NodeRemoved {
        /// The removed node.
        session_id: SessionId,
        /// The parent the node had before removal — `None` for a root — so a
        /// projection can drop the parent edge without an `EdgeRemoved` event.
        parent: Option<SessionId>,
    },
    /// An existing node moved to a new parent (from `reparent`).
    #[serde(rename_all = "camelCase")]
    EdgeReparented {
        /// The moved node (never the root).
        child: SessionId,
        /// The parent the node had before the move.
        old_parent: SessionId,
        /// The new parent (may equal `old_parent`, which updates the kind).
        new_parent: SessionId,
        /// The (possibly new) edge kind for the move.
        kind: EdgeKind,
    },
}
