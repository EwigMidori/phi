//! # phi-ext-tree-agent
//!
//! Session **tree** aggregate for [phi](https://github.com/ewigmidori/phi):
//! one root, exactly one parent per node, Live/Tombstoned lifecycle, injected
//! close policy, caller-projected tree events. Tree only — no graph.
//!
//! | Area | Surface |
//! |------|---------|
//! | Aggregate | [`SessionTree`] (`open`, `add_root`, `derive`, `close`, accessors) |
//! | Lifecycle | [`NodeState`], [`EdgeKind`], [`ParentEdge`], [`TreeEdge`] |
//! | Close rule | [`ClosePolicy`], [`NodeCloseContext`], [`CloseDisposition`], [`StandardClosePolicy`] |
//! | Persistence | [`TreeStore`], [`TreeSnapshot`], [`SnapshotNode`], [`InMemoryTreeStore`] |
//! | Events | [`TreeEvent`] (`nodeCreated` / `edgeCreated` / `nodeTombstoned` / `nodeRemoved`) |
//! | Errors | [`TreeError`] |
//!
//! **Not in this crate:** graphs (multi-parent / DAG / merge), cascade delete, a
//! built-in event bus, search (`continue_branch` / `branch` / `backtrack` /
//! `evaluate` with `TreeSearchPolicy` / `TreeEvaluator` / `TreeBudget` /
//! `SessionPort` land in v1), SQLite stores, `da-*` product logic beyond the
//! injected [`StandardClosePolicy`].
//!
//! **Design decision (audit-confirmed):** an early draft had a public
//! `TreeNode { session_id, state }` entity. The implementation converged on a
//! **private** `NodeRecord` inside the aggregate with projection through
//! [`SessionTree::node`] and the accessors — a public entity would repeat
//! `session_id` both in the entity and in every query parameter.
//!
//! **Mechanism vs strategy:** the tree owns topology and its invariants (one
//! root, single parent, no dangling edges, no cascade). Close behavior is a
//! *strategy* injected as [`ClosePolicy`] at construction; the tree still
//! enforces that no policy breaks topology (hard remove is honored for leaves
//! only). Mutations return [`Vec<TreeEvent>`] for the caller to project — there
//! is no event bus and nothing is persisted implicitly.
//!
//! **Naming:** the aggregate is [`SessionTree`], never a "graph" or a
//! "mailbox".

#![forbid(unsafe_code)]

pub mod error;
pub mod events;
pub mod model;
pub mod policy;
pub mod store;
pub mod tree;

pub use error::{Result, TreeError};
pub use events::TreeEvent;
pub use model::{EdgeKind, NodeState, ParentEdge, TreeEdge};
pub use policy::{CloseDisposition, ClosePolicy, NodeCloseContext, StandardClosePolicy};
pub use store::{InMemoryTreeStore, SnapshotNode, TreeSnapshot, TreeStore};
pub use tree::SessionTree;

#[cfg(test)]
mod integration {
    use std::sync::Arc;

    use phi_kernel::SessionId;

    use super::{EdgeKind, InMemoryTreeStore, SessionTree, StandardClosePolicy};

    #[test]
    fn public_api_end_to_end() {
        let store = Arc::new(InMemoryTreeStore::new());
        let tree = SessionTree::open(store.clone(), Arc::new(StandardClosePolicy)).unwrap();
        let root = SessionId::generate();
        tree.add_root(&root).unwrap();
        let (child, events) = tree.derive(&root, EdgeKind::WithHistory).unwrap();
        assert_eq!(events.len(), 2);
        tree.close(&child).unwrap();
        tree.persist().unwrap();
        let reopened = SessionTree::open(store, Arc::new(StandardClosePolicy)).unwrap();
        assert_eq!(reopened.root().unwrap(), Some(root));
    }
}
