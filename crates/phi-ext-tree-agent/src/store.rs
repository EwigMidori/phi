//! Persistence port for tree snapshots + an in-memory reference implementation.
//!
//! The port is intentionally **short and thick**: whole-snapshot `save` /
//! `load`. A SQLite store can be dropped in later without touching the
//! aggregate.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use phi_kernel::SessionId;

use crate::error::{Result, TreeError};
use crate::model::{NodeState, TreeEdge};

/// A single node as persisted in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotNode {
    /// The node's identity (reused from the kernel; no separate tree id type).
    pub session_id: SessionId,
    /// Live or Tombstoned at snapshot time.
    pub state: NodeState,
}

/// Serializable state of the whole tree: nodes + edges + states.
///
/// This is the persistence boundary: the aggregate snapshots itself into this
/// type and a [`TreeStore`] persists it. Structural validation lives here so
/// both the aggregate ([`crate::tree::SessionTree::open`]) and future stores
/// can rely on it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeSnapshot {
    /// All nodes.
    pub nodes: Vec<SnapshotNode>,
    /// All parent → child edges.
    pub edges: Vec<TreeEdge>,
}

impl TreeSnapshot {
    /// Validate the structural invariants the aggregate relies on: unique node
    /// ids; every edge endpoint is a node; at most one parent per child;
    /// exactly one root (none for an empty snapshot); every node reachable from
    /// the root (this catches cycles and orphaned components).
    pub fn validate(&self) -> Result<()> {
        let mut node_ids = BTreeSet::new();
        for node in &self.nodes {
            if !node_ids.insert(node.session_id.as_str()) {
                return Err(TreeError::InvalidSnapshot(format!(
                    "duplicate node: {}",
                    node.session_id
                )));
            }
        }
        if self.nodes.is_empty() {
            return if self.edges.is_empty() {
                Ok(())
            } else {
                Err(TreeError::InvalidSnapshot("edges without any nodes".into()))
            };
        }

        let mut children_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut parents_of: BTreeMap<&str, &str> = BTreeMap::new();
        for edge in &self.edges {
            if !node_ids.contains(edge.parent.as_str()) {
                return Err(TreeError::InvalidSnapshot(format!(
                    "edge parent is not a node: {}",
                    edge.parent
                )));
            }
            if !node_ids.contains(edge.child.as_str()) {
                return Err(TreeError::InvalidSnapshot(format!(
                    "edge child is not a node: {}",
                    edge.child
                )));
            }
            if parents_of
                .insert(edge.child.as_str(), edge.parent.as_str())
                .is_some()
            {
                return Err(TreeError::InvalidSnapshot(format!(
                    "child has multiple parents: {}",
                    edge.child
                )));
            }
            children_of
                .entry(edge.parent.as_str())
                .or_default()
                .push(edge.child.as_str());
        }

        let roots: Vec<&str> = node_ids
            .iter()
            .copied()
            .filter(|id| !parents_of.contains_key(id))
            .collect();
        if roots.len() != 1 {
            return Err(TreeError::InvalidSnapshot(format!(
                "expected exactly one root, found {}",
                roots.len()
            )));
        }
        let root = roots[0];

        let mut reached = BTreeSet::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !reached.insert(id) {
                continue;
            }
            if let Some(children) = children_of.get(id) {
                stack.extend(children.iter().copied());
            }
        }
        if reached.len() != node_ids.len() {
            return Err(TreeError::InvalidSnapshot(
                "nodes not reachable from the root (cycle or orphaned component)".into(),
            ));
        }
        Ok(())
    }
}

/// Persistence port: whole-snapshot `save` / `load`.
///
/// Short and thick on purpose — the aggregate rehydrates and persists in bulk,
/// and a future SQLite backend implements exactly these two methods.
pub trait TreeStore: Send + Sync {
    /// Overwrite the stored snapshot for this tree.
    fn save(&self, snapshot: &TreeSnapshot) -> Result<()>;

    /// Load the stored snapshot; `None` when the store is empty.
    fn load(&self) -> Result<Option<TreeSnapshot>>;
}

/// In-memory [`TreeStore`] (tests / scaffold only).
///
/// Single-snapshot slot: `save` overwrites, `load` returns the last saved
/// snapshot. Deliberately a dumb backend — validation happens at the call sites
/// ([`crate::tree::SessionTree::open`]), so this store never judges content.
#[derive(Clone, Debug, Default)]
pub struct InMemoryTreeStore {
    snapshot: Arc<Mutex<Option<TreeSnapshot>>>,
}

impl InMemoryTreeStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl TreeStore for InMemoryTreeStore {
    fn save(&self, snapshot: &TreeSnapshot) -> Result<()> {
        *self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snapshot.clone());
        Ok(())
    }

    fn load(&self) -> Result<Option<TreeSnapshot>> {
        Ok(self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EdgeKind;

    fn node(id: &str, state: NodeState) -> SnapshotNode {
        SnapshotNode {
            session_id: id.parse().unwrap(),
            state,
        }
    }

    fn edge(parent: &str, child: &str, kind: EdgeKind) -> TreeEdge {
        TreeEdge {
            parent: parent.parse().unwrap(),
            child: child.parse().unwrap(),
            kind,
        }
    }

    fn live(id: &str) -> SnapshotNode {
        node(id, NodeState::Live)
    }

    #[test]
    fn validate_accepts_a_well_formed_snapshot() {
        let snap = TreeSnapshot {
            nodes: vec![live("root"), node("a", NodeState::Tombstoned), live("b")],
            edges: vec![
                edge("root", "a", EdgeKind::WithHistory),
                edge("a", "b", EdgeKind::WithoutHistory),
            ],
        };
        snap.validate().unwrap();
    }

    #[test]
    fn validate_accepts_an_empty_snapshot() {
        TreeSnapshot {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn validate_rejects_duplicate_node_ids() {
        let snap = TreeSnapshot {
            nodes: vec![live("root"), live("root")],
            edges: Vec::new(),
        };
        assert!(matches!(
            snap.validate(),
            Err(TreeError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn validate_rejects_edges_without_nodes() {
        let snap = TreeSnapshot {
            nodes: Vec::new(),
            edges: vec![edge("root", "a", EdgeKind::WithHistory)],
        };
        assert!(matches!(
            snap.validate(),
            Err(TreeError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn validate_rejects_dangling_edges() {
        let snap = TreeSnapshot {
            nodes: vec![live("root")],
            edges: vec![edge("root", "ghost", EdgeKind::WithHistory)],
        };
        assert!(matches!(
            snap.validate(),
            Err(TreeError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn validate_rejects_multiple_roots() {
        let snap = TreeSnapshot {
            nodes: vec![live("root"), live("other")],
            edges: Vec::new(),
        };
        assert!(matches!(
            snap.validate(),
            Err(TreeError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn validate_rejects_a_child_with_two_parents() {
        let snap = TreeSnapshot {
            nodes: vec![live("root"), live("a"), live("b")],
            edges: vec![
                edge("root", "a", EdgeKind::WithHistory),
                edge("root", "b", EdgeKind::WithHistory),
                edge("a", "b", EdgeKind::WithoutHistory),
            ],
        };
        assert!(matches!(
            snap.validate(),
            Err(TreeError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn validate_rejects_an_unreachable_cycle() {
        let snap = TreeSnapshot {
            nodes: vec![live("root"), live("x")],
            edges: vec![edge("x", "x", EdgeKind::WithHistory)],
        };
        assert!(matches!(
            snap.validate(),
            Err(TreeError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn memory_store_roundtrips_a_snapshot() {
        let store = InMemoryTreeStore::new();
        assert_eq!(store.load().unwrap(), None);
        let snap = TreeSnapshot {
            nodes: vec![live("root")],
            edges: Vec::new(),
        };
        store.save(&snap).unwrap();
        assert_eq!(store.load().unwrap(), Some(snap));
    }
}
