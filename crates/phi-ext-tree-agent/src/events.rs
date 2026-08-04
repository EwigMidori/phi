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
/// [`crate::tree::SessionTree::close`] emits `[NodeTombstoned]` for a soft
/// delete or `[NodeRemoved]` for a hard remove; a no-op close emits nothing.
///
/// Field rationale (minimal projection set): a projection must be able to (a)
/// create a node, (b) link it to a parent with its edge kind, (c) flip a node's
/// state, and (d) drop a removed node *and* its parent edge. The last one is why
/// `NodeRemoved` carries `parent` — without it there would be no way to drop the
/// edge (there is deliberately no `EdgeRemoved` event in the minimal set).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TreeEvent {
    /// A new Live node appeared (from `add_root` or `derive`).
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
    /// A node became Tombstoned (close of a non-leaf; its edges stay).
    #[serde(rename_all = "camelCase")]
    NodeTombstoned {
        /// The node that was soft-deleted.
        session_id: SessionId,
    },
    /// A node was hard-removed with its incident edges (close of a leaf; no
    /// cascade into the subtree — a leaf has no subtree).
    #[serde(rename_all = "camelCase")]
    NodeRemoved {
        /// The removed node.
        session_id: SessionId,
        /// The parent the node had before removal — `None` for a root — so a
        /// projection can drop the parent edge without an `EdgeRemoved` event.
        parent: Option<SessionId>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn node_created_event_uses_camel_case_tag_and_fields() {
        let event = TreeEvent::NodeCreated {
            session_id: "s1".parse().unwrap(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], json!("nodeCreated"));
        assert_eq!(value["sessionId"], json!("s1"));
        let round: TreeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(round, event);
    }

    #[test]
    fn edge_created_event_roundtrips() {
        let event = TreeEvent::EdgeCreated {
            parent: "p".parse().unwrap(),
            child: "c".parse().unwrap(),
            kind: EdgeKind::WithoutHistory,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], json!("edgeCreated"));
        assert_eq!(value["kind"], json!("withoutHistory"));
        let round: TreeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(round, event);
    }

    #[test]
    fn tombstoned_and_removed_events_roundtrip() {
        let tombstoned = TreeEvent::NodeTombstoned {
            session_id: "s1".parse().unwrap(),
        };
        let value = serde_json::to_value(&tombstoned).unwrap();
        assert_eq!(value["type"], json!("nodeTombstoned"));
        let round: TreeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(round, tombstoned);

        let removed = TreeEvent::NodeRemoved {
            session_id: "s1".parse().unwrap(),
            parent: Some("p".parse().unwrap()),
        };
        let value = serde_json::to_value(&removed).unwrap();
        assert_eq!(value["type"], json!("nodeRemoved"));
        assert_eq!(value["parent"], json!("p"));
        let round: TreeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(round, removed);
    }
}
