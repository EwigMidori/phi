//! # phi-ext-tree-agent
//!
//! Session **tree** aggregate for [phi](https://github.com/ewigmidori/phi):
//! **pure topology + atomic, structure-preserving operations**. One root,
//! exactly one parent per node. Lifecycle state (Live/Tombstoned, close
//! semantics) is **not** a tree fact — products orchestrate it out-of-band
//! with `remove` / `remove_subtree` / `reparent`.
//!
//! | Area | Surface |
//! |------|---------|
//! | Aggregate | [`SessionTree`] (`open`, `add_root`, `derive`, `remove`, `remove_subtree`, `reparent`, accessors) |
//! | Model | [`EdgeKind`], [`ParentEdge`], [`TreeEdge`] |
//! | Persistence | [`TreeStore`], [`TreeSnapshot`], [`InMemoryTreeStore`] |
//! | Events | [`TreeEvent`] (`nodeCreated` / `edgeCreated` / `nodeRemoved` / `edgeReparented`) |
//! | Errors | [`TreeError`] |
//!
//! **Not in this crate:** lifecycle state (`NodeState` Live/Tombstoned) and
//! close policies — the product keeps them out-of-band and drives them with
//! the atomic operations; graphs (multi-parent / DAG / merge); implicit
//! cascade delete (only the explicit [`SessionTree::remove_subtree`]); a
//! built-in event bus; search (`continue_branch` / `branch` / `backtrack` /
//! `evaluate` with `TreeSearchPolicy` / `TreeEvaluator` / `TreeBudget` /
//! `SessionPort` land in v1); SQLite stores; `da-*` imports.
//!
//! **Mechanism vs strategy:** the tree owns topology and its invariants
//! (single parent, no dangling edges, single root, reachability) and only
//! exposes operations that preserve them: leaf-only `remove`, the explicit
//! atomic `remove_subtree` cascade, and acyclic `reparent`. Mutations return
//! [`Vec<TreeEvent>`] for the caller to project — there is no event bus and
//! nothing is persisted implicitly.
//!
//! **Design decision (audit-confirmed):** an early draft had a public
//! `TreeNode { session_id, state }` entity. The implementation converged on a
//! **private** `NodeRecord` inside the aggregate with projection through
//! [`SessionTree::contains`] and the accessors — a public entity would repeat
//! `session_id` both in the entity and in every query parameter.
//!
//! **Naming:** the aggregate is [`SessionTree`], never a "graph" or a
//! "mailbox".

#![forbid(unsafe_code)]

pub mod error;
pub mod events;
pub mod model;
pub mod store;
pub mod tree;

pub use error::{Result, TreeError};
pub use events::TreeEvent;
pub use model::{EdgeKind, ParentEdge, TreeEdge};
pub use store::{InMemoryTreeStore, TreeSnapshot, TreeStore};
pub use tree::SessionTree;

#[cfg(test)]
mod integration {
    use std::sync::Arc;

    use phi_kernel::SessionId;

    use super::{EdgeKind, InMemoryTreeStore, SessionTree};

    #[test]
    fn public_api_end_to_end() {
        let store = Arc::new(InMemoryTreeStore::new());
        let tree = SessionTree::open(store.clone()).unwrap();
        let root = SessionId::generate();
        tree.add_root(&root).unwrap();
        let (child, events) = tree.derive(&root, EdgeKind::WithHistory).unwrap();
        assert_eq!(events.len(), 2);
        assert!(tree.contains(&child));
        tree.remove(&child).unwrap();
        tree.persist().unwrap();
        let reopened = SessionTree::open(store).unwrap();
        assert_eq!(reopened.root().unwrap(), Some(root));
    }
}
