//! The [`SessionTree`] aggregate: pure topology + atomic, structure-preserving
//! operations.
//!
//! **Tree, not graph:** one root, every node has exactly one parent, no
//! multi-parent / DAG / merge.
//!
//! **Lifecycle is outside the tree:** there is no Live/Tombstoned state and no
//! close policy here. Products keep lifecycle out-of-band and drive it with
//! the atomic operations ([`Self::remove`] / [`Self::remove_subtree`] /
//! [`Self::reparent`]); the tree only guarantees that every operation preserves
//! its structural invariants (single parent, no dangling edges, single root,
//! reachability).
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
//!
//! **Id boundary:** the tree stores kernel [`SessionId`]s but never registers
//! them with the kernel — a node id is a tree fact, not necessarily a live
//! kernel session. Products must reconcile derived ids with the kernel
//! `SessionDirectory` when they realize a session.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use phi_kernel::SessionId;

use crate::error::{Result, TreeError};
use crate::events::TreeEvent;
use crate::model::{EdgeKind, ParentEdge, TreeEdge};
use crate::store::{TreeSnapshot, TreeStore};

/// Whether `node` is a **proper** descendant of `ancestor`.
///
/// Walks `node`'s parent chain upwards; the acyclic-tree invariant guarantees
/// the walk terminates at the root (returns `false`) or at `ancestor`
/// (returns `true`). Used by [`SessionTree::reparent`] for cycle detection.
fn is_descendant_of(
    map: &BTreeMap<String, NodeRecord>,
    ancestor: &SessionId,
    node: &SessionId,
) -> bool {
    let mut current = node.clone();
    while let Some(edge) = map
        .get(current.as_str())
        .and_then(|record| record.parent_edge.as_ref())
    {
        if edge.parent == *ancestor {
            return true;
        }
        current = edge.parent.clone();
    }
    false
}

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
    parent_edge: Option<ParentEdge>,
    children: Vec<SessionId>,
}

impl NodeRecord {
    /// A fresh root record (no parent edge).
    fn root(session_id: SessionId) -> Self {
        Self {
            session_id,
            parent_edge: None,
            children: Vec::new(),
        }
    }

    /// A fresh child record carrying its incoming edge.
    fn child(session_id: SessionId, parent: SessionId, kind: EdgeKind) -> Self {
        Self {
            session_id,
            parent_edge: Some(ParentEdge::new(parent, kind)),
            children: Vec::new(),
        }
    }
}

/// The session tree aggregate: pure topology + atomic operations.
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
/// **Design decision (audit-confirmed):** an early draft had a public
/// `TreeNode { session_id, state }` entity. The implementation converged on a
/// **private** `NodeRecord` inside the aggregate with projection through the
/// accessors — a public entity would repeat `session_id` both in the entity and
/// in every query parameter.
///
/// Construct with [`Self::open`]; mutate with [`Self::add_root`],
/// [`Self::derive`], [`Self::remove`], [`Self::remove_subtree`],
/// [`Self::reparent`]; read with the accessor methods; persist explicitly via
/// [`Self::persist`].
#[derive(Clone)]
pub struct SessionTree {
    /// Node id string → record (`BTreeMap` for deterministic snapshot order).
    nodes: Arc<Mutex<BTreeMap<String, NodeRecord>>>,
    /// Injected persistence port: loaded at `open`, written by `persist`.
    store: Arc<dyn TreeStore>,
}

impl SessionTree {
    /// Open (or create) the tree: when the injected store holds a snapshot it
    /// is loaded and validated, otherwise the tree starts empty. Named `open`
    /// because loading is unconditional behavior here, not a purely-fresh `new`.
    pub fn open(store: Arc<dyn TreeStore>) -> Result<Self> {
        let tree = Self {
            nodes: Arc::new(Mutex::new(BTreeMap::new())),
            store,
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
        for id in &snapshot.nodes {
            map.insert(
                id.as_str().to_owned(),
                NodeRecord {
                    session_id: id.clone(),
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

    /// Derive a new child from an existing parent.
    ///
    /// The tree **generates** the new id via [`SessionId::generate`] rather
    /// than taking it from the caller: callers cannot inject a duplicate or
    /// pre-existing id, and the id policy lives in exactly one place (the
    /// aggregate). The parent must **exist** (`NodeNotFound` otherwise) — the
    /// tree is pure topology, so any existing node can be derived from;
    /// lifecycle decisions are the product's.
    ///
    /// Emits `[NodeCreated, EdgeCreated]` (node first).
    pub fn derive(
        &self,
        parent: &SessionId,
        kind: EdgeKind,
    ) -> Result<(SessionId, Vec<TreeEvent>)> {
        let mut map = self.lock();
        if !map.contains_key(parent.as_str()) {
            return Err(TreeError::NodeNotFound(parent.to_string()));
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

    /// Remove a **leaf** node together with its parent edge.
    ///
    /// Structure-preserving: a non-leaf has children that would be orphaned
    /// (dangling edges), so removal is refused with `WouldOrphan` — use
    /// [`Self::remove_subtree`] for an explicit cascade.
    ///
    /// Emits `[NodeRemoved]`.
    pub fn remove(&self, session_id: &SessionId) -> Result<Vec<TreeEvent>> {
        let mut map = self.lock();
        let node = map
            .get(session_id.as_str())
            .ok_or_else(|| TreeError::NodeNotFound(session_id.to_string()))?;
        if !node.children.is_empty() {
            return Err(TreeError::WouldOrphan(session_id.to_string()));
        }
        let parent = node.parent_edge.as_ref().map(|edge| edge.parent.clone());
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

    /// Remove a node and **all of its descendants** atomically (single lock).
    ///
    /// Emits one `[NodeRemoved]` per removed node in **top-down** order
    /// (parents before their children, children in derivation order), each
    /// carrying its pre-removal parent — `None` for the tree root.
    pub fn remove_subtree(&self, session_id: &SessionId) -> Result<Vec<TreeEvent>> {
        let mut map = self.lock();
        if !map.contains_key(session_id.as_str()) {
            return Err(TreeError::NodeNotFound(session_id.to_string()));
        }
        // Detach the subtree root from its parent first: internal parents are
        // removed along with their children, so only this edge survives cleanup.
        let root_parent = map
            .get(session_id.as_str())
            .expect("existence checked above")
            .parent_edge
            .as_ref()
            .map(|edge| edge.parent.clone());
        if let Some(p) = root_parent.as_ref() {
            map.get_mut(p.as_str())
                .expect("the subtree root's parent is outside the subtree")
                .children
                .retain(|c| c != session_id);
        }
        // Collect the subtree top-down (BFS over the children lists).
        let mut order = vec![session_id.clone()];
        let mut cursor = 0;
        while cursor < order.len() {
            let node = map
                .get(order[cursor].as_str())
                .expect("subtree members exist until removed");
            order.extend(node.children.iter().cloned());
            cursor += 1;
        }
        // Emit one NodeRemoved per node, top-down, each with its pre-removal parent.
        let mut events = Vec::with_capacity(order.len());
        for id in &order {
            let parent = map
                .get(id.as_str())
                .expect("subtree members exist until removed")
                .parent_edge
                .as_ref()
                .map(|edge| edge.parent.clone());
            map.remove(id.as_str());
            events.push(TreeEvent::NodeRemoved {
                session_id: id.clone(),
                parent,
            });
        }
        Ok(events)
    }

    /// Move an existing non-root node to a new parent (structure preserving).
    ///
    /// The move is rejected when it would break invariants, in this check
    /// order: missing operand → `NodeNotFound`; the child is the root →
    /// `CannotReparentRoot` (the root has no parent by definition); the new
    /// parent is the child itself or a descendant of it → `WouldCycle` (a
    /// self-loop or an ancestor cycle). The child's incoming edge becomes
    /// `(new_parent, kind)`; the new parent's children list grows at the end
    /// (derivation order preserved). Reparenting to the node's **current**
    /// parent is allowed and acts as an edge-kind update.
    ///
    /// Emits `[EdgeReparented]`.
    pub fn reparent(
        &self,
        child: &SessionId,
        new_parent: &SessionId,
        kind: EdgeKind,
    ) -> Result<Vec<TreeEvent>> {
        let mut map = self.lock();
        if !map.contains_key(child.as_str()) {
            return Err(TreeError::NodeNotFound(child.to_string()));
        }
        if !map.contains_key(new_parent.as_str()) {
            return Err(TreeError::NodeNotFound(new_parent.to_string()));
        }
        if map
            .get(child.as_str())
            .expect("existence checked above")
            .parent_edge
            .is_none()
        {
            return Err(TreeError::CannotReparentRoot(child.to_string()));
        }
        if new_parent == child {
            return Err(TreeError::WouldCycle(format!(
                "{child} would become its own parent"
            )));
        }
        if is_descendant_of(&map, child, new_parent) {
            return Err(TreeError::WouldCycle(format!(
                "{new_parent} is inside {child}'s subtree"
            )));
        }
        let old_parent = map
            .get(child.as_str())
            .expect("existence checked above")
            .parent_edge
            .as_ref()
            .expect("non-root node always carries its parent edge")
            .parent
            .clone();
        map.get_mut(old_parent.as_str())
            .expect("a node's parent is always present")
            .children
            .retain(|c| c != child);
        map.get_mut(child.as_str())
            .expect("existence checked above")
            .parent_edge = Some(ParentEdge::new(new_parent.clone(), kind));
        map.get_mut(new_parent.as_str())
            .expect("existence checked above")
            .children
            .push(child.clone());
        Ok(vec![TreeEvent::EdgeReparented {
            child: child.clone(),
            old_parent,
            new_parent: new_parent.clone(),
            kind,
        }])
    }

    /// Whether the node exists. Never errors — after the removal of the
    /// state-returning `node()` accessor, callers use this for existence
    /// checks.
    #[must_use]
    pub fn contains(&self, session_id: &SessionId) -> bool {
        self.lock().contains_key(session_id.as_str())
    }

    /// The node's incoming edge: its parent and the edge kind; `None` for the
    /// root.
    ///
    /// In a single-parent tree the parent and the edge kind are **one fact**
    /// (they co-occur by construction), so one call returns the complete fact —
    /// the kind was not queryable before this accessor existed.
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

    /// Whether the node currently has no children (purely topological).
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

    /// Current full state as a snapshot (node ids + edges), ready for
    /// [`TreeStore::save`].
    ///
    /// Deterministic: nodes are emitted in ascending id order and edges grouped
    /// by parent with children in derivation order, so equal trees produce
    /// equal snapshots and the save/load round trip is lossless.
    #[must_use]
    pub fn snapshot(&self) -> TreeSnapshot {
        let map = self.lock();
        let nodes = map.values().map(|node| node.session_id.clone()).collect();
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
    ///
    /// **Point-in-time contract:** `persist` captures a **point-in-time
    /// snapshot of the tree at the moment of the call** — it is **not a
    /// consistency barrier**. The snapshot is cloned under the tree lock and
    /// then saved after the lock is released, so any concurrent mutation that
    /// lands after the snapshot is taken is **not** included in the saved
    /// state. Callers that need a consistent on-disk state coordinate it
    /// themselves (e.g. call during a no-concurrent-writers window, or guard
    /// with their own external barrier). The store I/O deliberately happens
    /// **outside** the tree lock — saving while holding the lock would block
    /// the whole tree behind the backend — and there is deliberately no
    /// write-through on mutations.
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
    use crate::store::InMemoryTreeStore;

    fn new_store() -> Arc<dyn TreeStore> {
        Arc::new(InMemoryTreeStore::new())
    }

    fn tree() -> SessionTree {
        SessionTree::open(new_store()).unwrap()
    }

    fn assert_valid(t: &SessionTree) {
        t.snapshot().validate().expect("snapshot stays valid");
    }

    #[test]
    fn add_root_creates_a_root_without_parent() {
        let t = tree();
        let root = SessionId::generate();
        assert_eq!(
            t.add_root(&root).unwrap(),
            vec![TreeEvent::NodeCreated {
                session_id: root.clone()
            }]
        );
        assert!(t.contains(&root));
        assert_eq!(t.incoming_edge(&root).unwrap(), None);
        assert!(t.children(&root).unwrap().is_empty());
        assert!(t.is_leaf(&root).unwrap());
        assert_eq!(t.path_to_root(&root).unwrap(), vec![root.clone()]);
        assert_eq!(t.root().unwrap(), Some(root));
        assert_valid(&t);
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
    fn derive_creates_a_child_under_an_existing_parent() {
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
        assert!(t.contains(&child));
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
    fn remove_a_leaf_removes_the_node_and_its_edge() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;

        assert_eq!(
            t.remove(&child).unwrap(),
            vec![TreeEvent::NodeRemoved {
                session_id: child.clone(),
                parent: Some(root.clone()),
            }]
        );
        assert!(!t.contains(&child));
        assert!(matches!(
            t.incoming_edge(&child),
            Err(TreeError::NodeNotFound(_))
        ));
        assert!(t.children(&root).unwrap().is_empty());
        // No dangling edges in the snapshot.
        let snap = t.snapshot();
        assert!(!snap.nodes.iter().any(|id| id == &child));
        assert!(!snap.edges.iter().any(|e| e.child == child));
        assert_valid(&t);
    }

    #[test]
    fn remove_a_non_leaf_is_refused() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let _child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        assert!(matches!(t.remove(&root), Err(TreeError::WouldOrphan(_))));
        // Nothing changed.
        assert!(t.contains(&root));
        assert_eq!(t.children(&root).unwrap().len(), 1);
        assert_valid(&t);
    }

    #[test]
    fn remove_a_missing_node_errors() {
        let t = tree();
        let ghost = SessionId::generate();
        assert!(matches!(t.remove(&ghost), Err(TreeError::NodeNotFound(_))));
    }

    #[test]
    fn remove_subtree_cascades_in_top_down_order() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let first_branch = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let second_branch = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        let left_leaf = t.derive(&first_branch, EdgeKind::WithHistory).unwrap().0;
        let right_leaf = t.derive(&first_branch, EdgeKind::WithoutHistory).unwrap().0;
        let lower_leaf = t.derive(&second_branch, EdgeKind::WithHistory).unwrap().0;

        assert_eq!(
            t.remove_subtree(&first_branch).unwrap(),
            vec![
                TreeEvent::NodeRemoved {
                    session_id: first_branch.clone(),
                    parent: Some(root.clone()),
                },
                TreeEvent::NodeRemoved {
                    session_id: left_leaf.clone(),
                    parent: Some(first_branch.clone()),
                },
                TreeEvent::NodeRemoved {
                    session_id: right_leaf.clone(),
                    parent: Some(first_branch.clone()),
                },
            ]
        );
        // first_branch and its descendants are gone; second_branch's subtree is untouched.
        assert!(!t.contains(&first_branch));
        assert!(!t.contains(&left_leaf));
        assert!(!t.contains(&right_leaf));
        assert!(t.contains(&second_branch));
        assert!(t.contains(&lower_leaf));
        assert_eq!(t.children(&root).unwrap(), vec![second_branch.clone()]);
        assert_valid(&t);
    }

    #[test]
    fn remove_subtree_of_the_root_empties_the_tree() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let a = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let leaf = t.derive(&a, EdgeKind::WithoutHistory).unwrap().0;

        assert_eq!(
            t.remove_subtree(&root).unwrap(),
            vec![
                TreeEvent::NodeRemoved {
                    session_id: root.clone(),
                    parent: None,
                },
                TreeEvent::NodeRemoved {
                    session_id: a.clone(),
                    parent: Some(root.clone()),
                },
                TreeEvent::NodeRemoved {
                    session_id: leaf.clone(),
                    parent: Some(a.clone()),
                },
            ]
        );
        assert_eq!(t.root().unwrap(), None);
        assert!(t.snapshot().nodes.is_empty());
        // The tree can host a new root again.
        let new_root = SessionId::generate();
        t.add_root(&new_root).unwrap();
        assert_eq!(t.root().unwrap(), Some(new_root));
    }

    #[test]
    fn remove_subtree_of_a_leaf_is_single_removal() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        assert_eq!(
            t.remove_subtree(&child).unwrap(),
            vec![TreeEvent::NodeRemoved {
                session_id: child.clone(),
                parent: Some(root.clone()),
            }]
        );
        assert!(t.children(&root).unwrap().is_empty());
    }

    #[test]
    fn remove_subtree_of_a_missing_node_errors() {
        let t = tree();
        let ghost = SessionId::generate();
        assert!(matches!(
            t.remove_subtree(&ghost),
            Err(TreeError::NodeNotFound(_))
        ));
    }

    #[test]
    fn reparent_moves_a_child_and_updates_edges() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let a = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let b = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        let x = t.derive(&a, EdgeKind::WithHistory).unwrap().0;

        assert_eq!(
            t.reparent(&x, &b, EdgeKind::WithoutHistory).unwrap(),
            vec![TreeEvent::EdgeReparented {
                child: x.clone(),
                old_parent: a.clone(),
                new_parent: b.clone(),
                kind: EdgeKind::WithoutHistory,
            }]
        );
        assert_eq!(
            t.incoming_edge(&x).unwrap(),
            Some(ParentEdge::new(b.clone(), EdgeKind::WithoutHistory))
        );
        assert!(t.children(&a).unwrap().is_empty());
        assert_eq!(t.children(&b).unwrap(), vec![x.clone()]);
        assert_eq!(
            t.path_to_root(&x).unwrap(),
            vec![x.clone(), b.clone(), root.clone()]
        );
        assert_valid(&t);
    }

    #[test]
    fn reparent_appends_to_the_new_parents_children() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let a = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let b = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        let b1 = t.derive(&b, EdgeKind::WithHistory).unwrap().0;
        let x = t.derive(&a, EdgeKind::WithHistory).unwrap().0;

        t.reparent(&x, &b, EdgeKind::WithoutHistory).unwrap();
        // The moved child is appended at the end of the new parent's list.
        assert_eq!(t.children(&b).unwrap(), vec![b1.clone(), x.clone()]);
    }

    #[test]
    fn reparent_to_the_current_parent_updates_the_edge_kind() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        t.reparent(&child, &root, EdgeKind::WithoutHistory).unwrap();
        assert_eq!(
            t.incoming_edge(&child).unwrap(),
            Some(ParentEdge::new(root.clone(), EdgeKind::WithoutHistory))
        );
        assert_eq!(t.children(&root).unwrap(), vec![child.clone()]);
    }

    #[test]
    fn reparent_the_root_is_refused() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        assert!(matches!(
            t.reparent(&root, &child, EdgeKind::WithHistory),
            Err(TreeError::CannotReparentRoot(_))
        ));
        // Nothing changed.
        assert_eq!(t.path_to_root(&child).unwrap(), vec![child, root]);
    }

    #[test]
    fn reparent_to_itself_is_refused() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        assert!(matches!(
            t.reparent(&child, &child, EdgeKind::WithHistory),
            Err(TreeError::WouldCycle(_))
        ));
        assert!(t.contains(&child));
    }

    #[test]
    fn reparent_into_its_own_subtree_is_refused() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let a = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let x = t.derive(&a, EdgeKind::WithHistory).unwrap().0;
        // Reparent `a` under its own descendant `x` would create a cycle.
        assert!(matches!(
            t.reparent(&a, &x, EdgeKind::WithHistory),
            Err(TreeError::WouldCycle(_))
        ));
        assert_eq!(t.path_to_root(&x).unwrap(), vec![x, a, root]);
    }

    #[test]
    fn reparent_with_missing_operands_errors() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let child = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let ghost = SessionId::generate();
        assert!(matches!(
            t.reparent(&ghost, &root, EdgeKind::WithHistory),
            Err(TreeError::NodeNotFound(_))
        ));
        assert!(matches!(
            t.reparent(&child, &ghost, EdgeKind::WithHistory),
            Err(TreeError::NodeNotFound(_))
        ));
    }

    #[test]
    fn every_mutation_leaves_a_valid_snapshot() {
        let t = tree();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let first_branch = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let second_branch = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        let left_leaf = t.derive(&first_branch, EdgeKind::WithHistory).unwrap().0;
        let right_leaf = t.derive(&first_branch, EdgeKind::WithoutHistory).unwrap().0;
        assert_valid(&t);

        t.remove(&left_leaf).unwrap();
        assert_valid(&t);
        // Failed remove changes nothing.
        assert!(matches!(
            t.remove(&first_branch),
            Err(TreeError::WouldOrphan(_))
        ));
        assert_valid(&t);
        // Failed reparent changes nothing.
        assert!(matches!(
            t.reparent(&first_branch, &right_leaf, EdgeKind::WithHistory),
            Err(TreeError::WouldCycle(_))
        ));
        assert_valid(&t);

        t.reparent(&first_branch, &second_branch, EdgeKind::WithHistory)
            .unwrap();
        assert_valid(&t);
        t.remove(&right_leaf).unwrap();
        assert_valid(&t);
        t.remove_subtree(&second_branch).unwrap();
        assert_valid(&t);
        t.remove(&root).unwrap();
        assert_valid(&t);

        assert!(t.snapshot().nodes.is_empty());
    }

    #[test]
    fn persist_and_reopen_roundtrips_the_tree() {
        let store = new_store();
        let t = SessionTree::open(store.clone()).unwrap();
        let root = SessionId::generate();
        t.add_root(&root).unwrap();
        let a = t.derive(&root, EdgeKind::WithHistory).unwrap().0;
        let b = t.derive(&root, EdgeKind::WithoutHistory).unwrap().0;
        let x = t.derive(&a, EdgeKind::WithHistory).unwrap().0;
        t.reparent(&x, &b, EdgeKind::WithoutHistory).unwrap();
        t.remove(&a).unwrap();
        t.persist().unwrap();

        let reopened = SessionTree::open(store).unwrap();
        assert_eq!(reopened.snapshot(), t.snapshot());
        assert_eq!(reopened.root().unwrap(), Some(root.clone()));
        assert_eq!(reopened.children(&root).unwrap(), vec![b.clone()]);
        assert_eq!(reopened.children(&b).unwrap(), vec![x.clone()]);
        assert_eq!(
            reopened.incoming_edge(&x).unwrap(),
            Some(ParentEdge::new(b.clone(), EdgeKind::WithoutHistory))
        );
        assert_eq!(reopened.path_to_root(&x).unwrap(), vec![x, b, root]);
    }

    #[test]
    fn persist_and_reopen_an_empty_tree() {
        let store = new_store();
        let t = SessionTree::open(store.clone()).unwrap();
        assert_eq!(t.root().unwrap(), None);
        t.persist().unwrap();
        let reopened = SessionTree::open(store).unwrap();
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
                "root".parse().unwrap(),
                "branch".parse().unwrap(),
                "leaf".parse().unwrap(),
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
        let t = SessionTree::open(store).unwrap();
        let root: SessionId = "root".parse().unwrap();
        let branch: SessionId = "branch".parse().unwrap();
        let leaf: SessionId = "leaf".parse().unwrap();
        assert!(t.contains(&branch));
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

    #[test]
    fn accessors_error_for_missing_nodes() {
        let t = tree();
        let ghost = SessionId::generate();
        assert!(!t.contains(&ghost));
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
}
