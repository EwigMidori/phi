//! The [`SessionTree`] aggregate: topology authority + lifecycle mutations.
//!
//! **Tree, not graph:** one root, every node has exactly one parent, no
//! multi-parent / DAG / merge. `derive` always creates a new Live node from a
//! Live parent.
//!
//! **Shared state:** internally `Arc<Mutex<…>>` with `&self` methods (the
//! kernel [`phi_kernel::SessionDirectory`] pattern). `Clone` shares one logical
//! tree cheaply, and v1 search (`continue_branch` / `branch` / `backtrack` /
//! `evaluate`) will run from the pump while HTTP handlers read the same tree —
//! a `&mut` aggregate would make that composition awkward.
//!
//! **Persistence:** [`Self::open`] rehydrates + validates a snapshot from the
//! injected [`TreeStore`]; mutations never persist implicitly — call
//! [`Self::persist`] (or [`Self::snapshot`] + a [`TreeStore`]) explicitly.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use phi_kernel::SessionId;

use crate::error::{Result, TreeError};
use crate::events::TreeEvent;
use crate::model::{EdgeKind, NodeState, ParentEdge, TreeEdge};
use crate::policy::{CloseDisposition, ClosePolicy, NodeCloseContext};
use crate::store::{SnapshotNode, TreeSnapshot, TreeStore};

/// Internal per-node record.
///
/// The incoming edge lives on the child (single-parent tree): `parent_edge`
/// describes the child's edge to its parent, and `children` is the reverse
/// index. Both halves are mutated together under the tree lock, so the
/// single-parent invariant cannot drift.
///
/// **Key invariant:** the owning `BTreeMap<String, NodeRecord>` key is always
/// `record.session_id.as_str().to_owned()`. `String` keys are used because the
/// kernel's `SessionId` has no `Ord`; the `BTreeMap` ordering is what makes
/// snapshots deterministic.
#[derive(Clone, Debug)]
struct NodeRecord {
    session_id: SessionId,
    state: NodeState,
    parent_edge: Option<ParentEdge>,
    children: Vec<SessionId>,
}

impl NodeRecord {
    /// A fresh root record (no parent edge).
    fn root(session_id: SessionId) -> Self {
        Self {
            session_id,
            state: NodeState::Live,
            parent_edge: None,
            children: Vec::new(),
        }
    }

    /// A fresh Live child record carrying its incoming edge.
    fn child(session_id: SessionId, parent: SessionId, kind: EdgeKind) -> Self {
        Self {
            session_id,
            state: NodeState::Live,
            parent_edge: Some(ParentEdge::new(parent, kind)),
            children: Vec::new(),
        }
    }
}

/// The session tree aggregate: topology authority + lifecycle mutations.
///
/// **Why `Arc<Mutex<…>>` + `&self` instead of a plain `&mut` struct:** the
/// kernel's [`phi_kernel::SessionDirectory`] sets the precedent, and v1 search
/// runs from the pump while other handlers read the same tree — shared `&self`
/// access keeps that composition trivial and avoids splitting the aggregate.
/// `Clone` is cheap and shares the one logical tree; it is not a copy.
///
/// The internal map is a `BTreeMap` keyed by the node's id string so that
/// [`Self::snapshot`] emits nodes and edges in a stable, deterministic order
/// (equal trees produce equal snapshots, which tests and diffs can rely on).
///
/// Construct with [`Self::open`]; mutate with [`Self::add_root`],
/// [`Self::derive`], [`Self::close`]; read with the accessor methods; persist
/// explicitly via [`Self::persist`].
#[derive(Clone)]
pub struct SessionTree {
    /// Node id string → record (`BTreeMap` for deterministic snapshot order).
    nodes: Arc<Mutex<BTreeMap<String, NodeRecord>>>,
    /// Injected persistence port: loaded at `open`, written by `persist`.
    store: Arc<dyn TreeStore>,
    /// Injected close strategy (explicit at construction; never a default).
    close_policy: Arc<dyn ClosePolicy>,
}

impl SessionTree {
    /// Open (or create) the tree: when the injected store holds a snapshot it
    /// is loaded and validated, otherwise the tree starts empty. Named `open`
    /// because loading is unconditional behavior here, not a purely-fresh `new`.
    ///
    /// `close_policy` is injected at this point — explicit construction, no
    /// [`Default`] (a missing policy is a caller bug, not a silent fallback).
    pub fn open(store: Arc<dyn TreeStore>, close_policy: Arc<dyn ClosePolicy>) -> Result<Self> {
        let tree = Self {
            nodes: Arc::new(Mutex::new(BTreeMap::new())),
            store,
            close_policy,
        };
        if let Some(snapshot) = tree.store.load()? {
            snapshot.validate()?;
            tree.rebuild_from(&snapshot);
        }
        Ok(tree)
    }

    /// Lock the node map, recovering from a poisoned lock (kernel convention: a
    /// panicked writer must not wedge the whole tree).
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, NodeRecord>> {
        self.nodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Rebuild the in-memory map from a validated snapshot.
    ///
    /// All nodes are inserted first so every parent exists when edges are
    /// linked; children are pushed in snapshot edge order, which
    /// [`Self::snapshot`] emits in derivation order — the save/load round trip
    /// is lossless.
    fn rebuild_from(&self, snapshot: &TreeSnapshot) {
        let mut map = self.lock();
        for node in &snapshot.nodes {
            map.insert(
                node.session_id.as_str().to_owned(),
                NodeRecord {
                    session_id: node.session_id.clone(),
                    state: node.state,
                    parent_edge: None,
                    children: Vec::new(),
                },
            );
        }
        for edge in &snapshot.edges {
            let child = map
                .get_mut(edge.child.as_str())
                .expect("validated snapshot links only existing nodes");
            child.parent_edge = Some(ParentEdge::new(edge.parent.clone(), edge.kind));
            map.get_mut(edge.parent.as_str())
                .expect("validated snapshot links only existing nodes")
                .children
                .push(edge.child.clone());
        }
    }

    /// Create the root node (no parent).
    ///
    /// Duplicates are **loud errors, not silent no-ops**: an id already in the
    /// tree fails with `NodeAlreadyExists`, and any other id when a root exists
    /// fails with `RootAlreadyExists` — a tree has exactly one root, and an
    /// idempotent no-op would hide double-creation bugs. Guard retries at the
    /// call site.
    ///
    /// Emits `[NodeCreated]`.
    pub fn add_root(&self, session_id: &SessionId) -> Result<Vec<TreeEvent>> {
        let mut map = self.lock();
        if map.get(session_id.as_str()).is_some() {
            return Err(TreeError::NodeAlreadyExists(session_id.to_string()));
        }
        if map.values().any(|node| node.parent_edge.is_none()) {
            return Err(TreeError::RootAlreadyExists(session_id.to_string()));
        }
        map.insert(
            session_id.as_str().to_owned(),
            NodeRecord::root(session_id.clone()),
        );
        Ok(vec![TreeEvent::NodeCreated {
            session_id: session_id.clone(),
        }])
    }

    /// Derive a new Live child from a Live parent.
    ///
    /// The tree **generates** the new id via [`SessionId::generate`] rather
    /// than taking it from the caller: callers cannot inject a duplicate or
    /// pre-existing id, and the id policy lives in exactly one place (the
    /// aggregate). The parent must exist and be Live; the `kind` parameter is
    /// explicit (no default history behavior).
    ///
    /// Emits `[NodeCreated, EdgeCreated]` (node first).
    pub fn derive(
        &self,
        parent: &SessionId,
        kind: EdgeKind,
    ) -> Result<(SessionId, Vec<TreeEvent>)> {
        let mut map = self.lock();
        let parent_record = map
            .get(parent.as_str())
            .ok_or_else(|| TreeError::NodeNotFound(parent.to_string()))?;
        if parent_record.state != NodeState::Live {
            return Err(TreeError::NodeNotLive(parent.to_string()));
        }
        let child = SessionId::generate();
        map.insert(
            child.as_str().to_owned(),
            NodeRecord::child(child.clone(), parent.clone(), kind),
        );
        map.get_mut(parent.as_str())
            .expect("parent existence checked above")
            .children
            .push(child.clone());
        let events = vec![
            TreeEvent::NodeCreated {
                session_id: child.clone(),
            },
            TreeEvent::EdgeCreated {
                parent: parent.clone(),
                child: child.clone(),
                kind,
            },
        ];
        Ok((child, events))
    }

    /// Close a node according to the injected [`ClosePolicy`].
    ///
    /// The tree resolves topology facts (leaf-ness) and hands them to the
    /// policy via [`NodeCloseContext`], then enforces its invariants on the
    /// policy's answer: `HardRemove` is honored for leaves only — removing a
    /// non-leaf would orphan its children, so the tree refuses with
    /// `CloseRefused` no matter what the policy asked. A close that changes
    /// nothing (an already-Tombstoned non-leaf) emits no events.
    ///
    /// Emits `[NodeTombstoned]` or `[NodeRemoved]`.
    pub fn close(&self, session_id: &SessionId) -> Result<Vec<TreeEvent>> {
        let mut map = self.lock();
        let (state, is_leaf) = {
            let node = map
                .get(session_id.as_str())
                .ok_or_else(|| TreeError::NodeNotFound(session_id.to_string()))?;
            (node.state, node.children.is_empty())
        };
        match self
            .close_policy
            .disposition(&NodeCloseContext::new(state, is_leaf))
        {
            CloseDisposition::Tombstone => {
                if state == NodeState::Tombstoned {
                    // Already soft-deleted: no change → no event.
                    return Ok(Vec::new());
                }
                let node = map
                    .get_mut(session_id.as_str())
                    .expect("node existence checked above");
                node.state = NodeState::Tombstoned;
                Ok(vec![TreeEvent::NodeTombstoned {
                    session_id: session_id.clone(),
                }])
            }
            CloseDisposition::HardRemove => {
                if !is_leaf {
                    return Err(TreeError::CloseRefused(format!(
                        "hard remove of a non-leaf would orphan its children: {session_id}"
                    )));
                }
                let parent = map
                    .get(session_id.as_str())
                    .expect("node existence checked above")
                    .parent_edge
                    .as_ref()
                    .map(|edge| edge.parent.clone());
                map.remove(session_id.as_str());
                if let Some(p) = parent.as_ref() {
                    map.get_mut(p.as_str())
                        .expect("a node's parent is always present")
                        .children
                        .retain(|c| c != session_id);
                }
                Ok(vec![TreeEvent::NodeRemoved {
                    session_id: session_id.clone(),
                    parent,
                }])
            }
        }
    }

    /// The node's current state.
    ///
    /// A missing node is an error (`NodeNotFound`), so callers that only want
    /// existence can test with `node(...).is_ok()`.
    pub fn node(&self, session_id: &SessionId) -> Result<NodeState> {
        self.lock()
            .get(session_id.as_str())
            .map(|node| node.state)
            .ok_or_else(|| TreeError::NodeNotFound(session_id.to_string()))
    }

    /// The node's incoming edge: its parent and the edge kind; `None` for the
    /// root.
    ///
    /// Replaces a bare-parent accessor: in a single-parent tree the parent and
    /// the edge kind are **one fact** (they co-occur by construction), so one
    /// call returns the complete fact — the kind was not queryable before.
    ///
    /// A missing node is an error (`NodeNotFound`).
    pub fn incoming_edge(&self, session_id: &SessionId) -> Result<Option<ParentEdge>> {
        self.lock()
            .get(session_id.as_str())
            .map(|node| node.parent_edge.clone())
            .ok_or_else(|| TreeError::NodeNotFound(session_id.to_string()))
    }

    /// The node's children, in derivation order.
    ///
    /// A missing node is an error (`NodeNotFound`). Order is preserved across a
    /// save/load round trip (see [`Self::snapshot`]).
    pub fn children(&self, session_id: &SessionId) -> Result<Vec<SessionId>> {
        self.lock()
            .get(session_id.as_str())
            .map(|node| node.children.clone())
            .ok_or_else(|| TreeError::NodeNotFound(session_id.to_string()))
    }

    /// The path from the node up to the root (node first, root last).
    ///
    /// A missing node is an error (`NodeNotFound`). The parent chain is acyclic
    /// by the tree invariant, so this always terminates.
    pub fn path_to_root(&self, session_id: &SessionId) -> Result<Vec<SessionId>> {
        let map = self.lock();
        let mut path = Vec::new();
        let mut current = session_id.clone();
        loop {
            let node = map
                .get(current.as_str())
                .ok_or_else(|| TreeError::NodeNotFound(current.to_string()))?;
            path.push(current.clone());
            match node.parent_edge.as_ref() {
                Some(edge) => current = edge.parent.clone(),
                None => return Ok(path),
            }
        }
    }

    /// Whether the node currently has no children (purely topological — a
    /// Tombstoned node with no children is still a leaf).
    ///
    /// A missing node is an error (`NodeNotFound`).
    pub fn is_leaf(&self, session_id: &SessionId) -> Result<bool> {
        self.lock()
            .get(session_id.as_str())
            .map(|node| node.children.is_empty())
            .ok_or_else(|| TreeError::NodeNotFound(session_id.to_string()))
    }

    /// The tree's root node; `Ok(None)` when the tree is empty.
    pub fn root(&self) -> Result<Option<SessionId>> {
        let map = self.lock();
        Ok(map
            .values()
            .find(|node| node.parent_edge.is_none())
            .map(|node| node.session_id.clone()))
    }

    /// Current full state as a snapshot (nodes + edges), ready for
    /// [`TreeStore::save`].
    ///
    /// Deterministic: nodes are emitted in ascending id order and edges grouped
    /// by parent with children in derivation order, so equal trees produce
    /// equal snapshots and the save/load round trip is lossless.
    #[must_use]
    pub fn snapshot(&self) -> TreeSnapshot {
        let map = self.lock();
        let nodes = map
            .values()
            .map(|node| SnapshotNode {
                session_id: node.session_id.clone(),
                state: node.state,
            })
            .collect();
        let edges = map
            .values()
            .flat_map(|parent| {
                parent
                    .children
                    .iter()
                    .map(|child| {
                        let child_record = map
                            .get(child.as_str())
                            .expect("children lists only contain existing nodes");
                        TreeEdge {
                            parent: parent.session_id.clone(),
                            child: child.clone(),
                            kind: child_record
                                .parent_edge
                                .as_ref()
                                .expect("every non-root node stores its parent edge")
                                .kind,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        TreeSnapshot { nodes, edges }
    }

    /// Persist the current state through the injected store.
    ///
    /// Explicit only — mutations never write implicitly (same stance as the
    /// events: the caller projects and persists).
    pub fn persist(&self) -> Result<()> {
        let snapshot = self.snapshot();
        self.store.save(&snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::policy::StandardClosePolicy;
    use crate::store::InMemoryTreeStore;

    /// Always-hard-remove policy, to prove the tree enforces invariants
    /// against custom policies.
    struct AlwaysHardRemove;

    impl ClosePolicy for AlwaysHardRemove {
        fn disposition(&self, _ctx: &NodeCloseContext) -> CloseDisposition {
            CloseDisposition::HardRemove
        }
    }

    fn new_store() -> Arc<dyn TreeStore> {
        Arc::new(InMemoryTreeStore::new())
    }

    fn standard() -> Arc<dyn ClosePolicy> {
        Arc::new(StandardClosePolicy)
    }

    fn tree() -> SessionTree {
        SessionTree::open(new_store(), standard()).unwrap()
    }

    #[test]
    fn add_root_creates_a_live_root_without_parent() {
        let t = tree();
        let root = SessionId::generate();
        assert_eq!(
            t.add_root(&root).unwrap(),
            vec![TreeEvent::NodeCreated {
                session_id: root.clone()
            }]
        );
        assert_eq!(t.node(&root).unwrap(), NodeState::Live);
        assert_eq!(t.incoming_edge(&root).unwrap(), None);
        assert!(t.children(&root).unwrap().is_empty());
        assert!(t.is_leaf(&root).unwrap());
        assert_eq!(t.path_to_root(&root).unwrap(), vec![root.clone()]);
        assert_eq!(t.root().unwrap(), Some(root));
    }

    #[test]
    fn add_root_duplicates_are_loud_errors() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        // Same id again → the node already exists.
        assert!(matches!(
            t.add_root(&root),
            Err(TreeError::NodeAlreadyExists(_))
        ));
        // A different id while a root exists → a tree has exactly one root.
        let other = SessionId::generate();
        assert!(matches!(
            t.add_root(&other),
            Err(TreeError::RootAlreadyExists(_))
        ));
        // State untouched.
        assert_eq!(t.root().unwrap(), Some(root));
    }

    #[test]
    fn derive_creates_a_live_child_under_a_live_parent() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let (child, events) = t.derive(&root, EdgeKind::WithHistory).unwrap();
        assert_eq!(
            events,
            vec![
                TreeEvent::NodeCreated {
                    session_id: child.clone()
                },
                TreeEvent::EdgeCreated {
                    parent: root.clone(),
                    child: child.clone(),
                    kind: EdgeKind::WithHistory,
                },
            ]
        );
        assert_eq!(t.node(&child).unwrap(), NodeState::Live);
        assert_eq!(
            t.incoming_edge(&child).unwrap(),
            Some(ParentEdge::new(root.clone(), EdgeKind::WithHistory))
        );
        assert_eq!(t.children(&root).unwrap(), vec![child.clone()]);
        assert!(!t.is_leaf(&root).unwrap());
        assert!(t.is_leaf(&child).unwrap());
        assert_eq!(
            t.path_to_root(&child).unwrap(),
            vec![child.clone(), root.clone()]
        );
    }

    #[test]
    fn derive_twice_creates_two_distinct_children_in_order() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let (first, _) = t.derive(&root, EdgeKind::WithHistory).unwrap();
        let (second, _) = t.derive(&root, EdgeKind::WithoutHistory).unwrap();
        assert_ne!(first, second);
        assert_eq!(t.children(&root).unwrap(), vec![first, second]);
    }

    #[test]
    fn derive_from_a_missing_parent_fails() {
        let t = tree();
        let ghost = SessionId::generate();
        assert!(matches!(
            t.derive(&ghost, EdgeKind::WithHistory),
            Err(TreeError::NodeNotFound(_))
        ));
    }

    #[test]
    fn derive_from_a_tombstoned_parent_fails() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        // The root is now a non-leaf: closing it tombstones it.
        t.close(&root).unwrap();
        assert_eq!(t.node(&root).unwrap(), NodeState::Tombstoned);
        assert!(matches!(
            t.derive(&root, EdgeKind::WithHistory),
            Err(TreeError::NodeNotLive(_))
        ));
        // The existing subtree is untouched.
        assert_eq!(
            t.incoming_edge(&child).unwrap(),
            Some(ParentEdge::new(root.clone(), EdgeKind::WithoutHistory))
        );
    }

    #[test]
    fn close_a_leaf_hard_removes_it_and_its_edge() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;

        assert_eq!(
            t.close(&child).unwrap(),
            vec![TreeEvent::NodeRemoved {
                session_id: child.clone(),
                parent: Some(root.clone()),
            }]
        );
        // The node is gone and accessors now error.
        assert!(matches!(t.node(&child), Err(TreeError::NodeNotFound(_))));
        assert!(matches!(
            t.incoming_edge(&child),
            Err(TreeError::NodeNotFound(_))
        ));
        // The parent no longer lists it and the snapshot has no dangling edge.
        assert!(t.children(&root).unwrap().is_empty());
        let snap = t.snapshot();
        assert!(!snap.nodes.iter().any(|n| n.session_id == child));
        assert!(!snap.edges.iter().any(|e| e.child == child));
    }

    #[test]
    fn close_a_non_leaf_tombstones_it_and_keeps_the_subtree_connected() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let branch = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let leaf = t.derive(&branch, EdgeKind::WithoutHistory).unwrap().0;

        assert_eq!(
            t.close(&branch).unwrap(),
            vec![TreeEvent::NodeTombstoned {
                session_id: branch.clone()
            }]
        );
        assert_eq!(t.node(&branch).unwrap(), NodeState::Tombstoned);
        // Soft delete keeps the topology: the leaf is still reachable.
        assert_eq!(t.children(&branch).unwrap(), vec![leaf.clone()]);
        assert_eq!(
            t.incoming_edge(&leaf).unwrap(),
            Some(ParentEdge::new(branch.clone(), EdgeKind::WithoutHistory))
        );
        assert_eq!(
            t.path_to_root(&leaf).unwrap(),
            vec![leaf.clone(), branch.clone(), root.clone()]
        );
        assert_eq!(t.snapshot().edges.len(), 2);
    }

    #[test]
    fn close_an_already_tombstoned_non_leaf_is_a_noop_without_events() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let branch = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let _leaf = t.derive(&branch, EdgeKind::WithHistory).unwrap().0;
        t.close(&branch).unwrap();
        // Closing again changes nothing → no ghost event.
        assert_eq!(t.close(&branch).unwrap(), Vec::<TreeEvent>::new());
        assert_eq!(t.node(&branch).unwrap(), NodeState::Tombstoned);
        assert_eq!(t.children(&branch).unwrap().len(), 1);
    }

    #[test]
    fn a_tombstoned_leaf_is_hard_removed_on_close() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let branch = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let leaf = t.derive(&branch, EdgeKind::WithHistory).unwrap().0;

        // Tombstone the non-leaf branch, then remove its live leaf child.
        t.close(&branch).unwrap();
        t.close(&leaf).unwrap();
        // The branch is now a Tombstoned leaf → closing hard-removes it.
        assert_eq!(t.node(&branch).unwrap(), NodeState::Tombstoned);
        assert_eq!(
            t.close(&branch).unwrap(),
            vec![TreeEvent::NodeRemoved {
                session_id: branch.clone(),
                parent: Some(root.clone()),
            }]
        );
        assert!(matches!(t.node(&branch), Err(TreeError::NodeNotFound(_))));
        assert!(t.children(&root).unwrap().is_empty());
    }

    #[test]
    fn closing_the_root_leaf_empties_the_tree_and_allows_a_new_root() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        assert_eq!(
            t.close(&root).unwrap(),
            vec![TreeEvent::NodeRemoved {
                session_id: root.clone(),
                parent: None,
            }]
        );
        assert_eq!(t.root().unwrap(), None);
        assert!(t.snapshot().nodes.is_empty());
        let new_root = SessionId::generate();
        t.add_root(&new_root).unwrap();
        assert_eq!(t.root().unwrap(), Some(new_root));
    }

    #[test]
    fn close_of_a_missing_node_errors() {
        let t = tree();
        let ghost = SessionId::generate();
        assert!(matches!(t.close(&ghost), Err(TreeError::NodeNotFound(_))));
    }

    #[test]
    fn accessors_error_for_missing_nodes() {
        let t = tree();
        let ghost = SessionId::generate();
        assert!(matches!(t.node(&ghost), Err(TreeError::NodeNotFound(_))));
        assert!(matches!(
            t.incoming_edge(&ghost),
            Err(TreeError::NodeNotFound(_))
        ));
        assert!(matches!(
            t.children(&ghost),
            Err(TreeError::NodeNotFound(_))
        ));
        assert!(matches!(t.is_leaf(&ghost), Err(TreeError::NodeNotFound(_))));
        assert!(matches!(
            t.path_to_root(&ghost),
            Err(TreeError::NodeNotFound(_))
        ));
    }

    #[test]
    fn hard_remove_of_a_non_leaf_is_refused_even_for_custom_policies() {
        let store = new_store();
        let t = SessionTree::open(store, Arc::new(AlwaysHardRemove)).unwrap();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        // Non-leaf hard remove would orphan the subtree → the tree refuses.
        assert!(matches!(t.close(&root), Err(TreeError::CloseRefused(_))));
        // Leaf hard remove is honored.
        assert_eq!(t.close(&child).unwrap().len(), 1);
    }

    #[test]
    fn every_mutation_leaves_a_valid_snapshot() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let branch_a = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let branch_b = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        let leaf = t.derive(&branch_a, EdgeKind::WithHistory).unwrap().0;
        let ids = [root.clone(), branch_a.clone(), branch_b, leaf];
        for (step, id) in ids.iter().enumerate() {
            t.close(id).unwrap();
            assert!(
                t.snapshot().validate().is_ok(),
                "snapshot must stay valid after step {step}"
            );
        }
        // Soft delete kept the subtree connected: root(T) still holds branch_a(T).
        assert_eq!(t.snapshot().nodes.len(), 2);
        assert_eq!(t.snapshot().edges.len(), 1);
        assert_eq!(t.node(&branch_a).unwrap(), NodeState::Tombstoned);
        // Tombstoned leaves are hard-removed by a later close, emptying the tree.
        t.close(&branch_a).unwrap();
        assert!(t.snapshot().validate().is_ok());
        t.close(&root).unwrap();
        assert!(t.snapshot().validate().is_ok());
        assert!(t.snapshot().nodes.is_empty());
    }

    #[test]
    fn persist_and_reopen_roundtrips_the_tree() {
        let store = new_store();
        let t = SessionTree::open(store.clone(), standard()).unwrap();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let branch = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let leaf = t.derive(&branch, EdgeKind::WithoutHistory).unwrap().0;
        t.close(&branch).unwrap(); // tombstone; the subtree is kept
        t.persist().unwrap();

        let reopened = SessionTree::open(store, standard()).unwrap();
        assert_eq!(reopened.snapshot(), t.snapshot());
        assert_eq!(reopened.root().unwrap(), Some(root.clone()));
        assert_eq!(reopened.node(&branch).unwrap(), NodeState::Tombstoned);
        assert_eq!(reopened.children(&branch).unwrap(), vec![leaf.clone()]);
        assert_eq!(
            reopened.incoming_edge(&leaf).unwrap(),
            Some(ParentEdge::new(branch.clone(), EdgeKind::WithoutHistory))
        );
        assert_eq!(
            reopened.path_to_root(&leaf).unwrap(),
            vec![leaf.clone(), branch, root]
        );
    }

    #[test]
    fn persist_and_reopen_an_empty_tree() {
        let store = new_store();
        let t = SessionTree::open(store.clone(), standard()).unwrap();
        assert_eq!(t.root().unwrap(), None);
        t.persist().unwrap();
        let reopened = SessionTree::open(store, standard()).unwrap();
        assert_eq!(reopened.root().unwrap(), None);
        let root = SessionId::generate();
        reopened.add_root(&root).unwrap();
        assert_eq!(reopened.root().unwrap(), Some(root));
    }

    #[test]
    fn open_rehydrates_from_a_hand_built_snapshot() {
        let store = new_store();
        let snapshot = TreeSnapshot {
            nodes: vec![
                SnapshotNode {
                    session_id: "root".parse().unwrap(),
                    state: NodeState::Live,
                },
                SnapshotNode {
                    session_id: "branch".parse().unwrap(),
                    state: NodeState::Tombstoned,
                },
                SnapshotNode {
                    session_id: "leaf".parse().unwrap(),
                    state: NodeState::Live,
                },
            ],
            edges: vec![
                TreeEdge {
                    parent: "root".parse().unwrap(),
                    child: "branch".parse().unwrap(),
                    kind: EdgeKind::WithHistory,
                },
                TreeEdge {
                    parent: "branch".parse().unwrap(),
                    child: "leaf".parse().unwrap(),
                    kind: EdgeKind::WithoutHistory,
                },
            ],
        };
        store.save(&snapshot).unwrap();
        let t = SessionTree::open(store, standard()).unwrap();
        let root: SessionId = "root".parse().unwrap();
        let branch: SessionId = "branch".parse().unwrap();
        let leaf: SessionId = "leaf".parse().unwrap();
        assert_eq!(t.node(&branch).unwrap(), NodeState::Tombstoned);
        assert_eq!(t.children(&root).unwrap(), vec![branch.clone()]);
        assert_eq!(
            t.incoming_edge(&leaf).unwrap(),
            Some(ParentEdge::new(branch.clone(), EdgeKind::WithoutHistory))
        );
        assert_eq!(t.path_to_root(&leaf).unwrap(), vec![leaf, branch, root]);
    }

    #[test]
    fn snapshots_serialize_deterministically() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        t.derive(&root, EdgeKind::WithHistory).unwrap();
        let first = t.snapshot();
        let second = t.snapshot();
        assert_eq!(first, second);
        let json = serde_json::to_string(&first).unwrap();
        assert_eq!(serde_json::to_string(&second).unwrap(), json);
    }

    #[test]
    fn concurrent_add_root_keeps_exactly_one_root() {
        let t = tree();
        let first = SessionId::generate();
        let second = SessionId::generate();
        let shared_a = t.clone();
        let shared_b = t.clone();
        let handle_a = thread::spawn(move || shared_a.add_root(&first));
        let handle_b = thread::spawn(move || shared_b.add_root(&second));
        let results = [handle_a.join().unwrap(), handle_b.join().unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .any(|r| matches!(r, Err(TreeError::RootAlreadyExists(_))))
        );
        assert!(t.root().unwrap().is_some());
    }
}
