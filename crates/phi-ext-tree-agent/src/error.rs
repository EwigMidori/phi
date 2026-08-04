//! Crate-local errors for the session tree.

use thiserror::Error;

/// Errors from [`crate::tree::SessionTree`] operations.
///
/// This crate keeps its own error type instead of reusing the kernel's
/// [`phi_kernel::KernelError`]: the tree contract is about topology, not the
/// kernel's generation queue. The shape mirrors kernel conventions
/// (not-found style messages).
///
/// There is deliberately **no lifecycle error**: the tree is pure topology, so
/// "not live" is not a tree concept — lifecycle state lives product-side.
#[derive(Debug, Error)]
pub enum TreeError {
    /// The session id is not a node in the tree (any missing operand).
    #[error("node not found: {0}")]
    NodeNotFound(String),

    /// The id is already a node (duplicate [`crate::tree::SessionTree::add_root`]).
    #[error("node already exists: {0}")]
    NodeAlreadyExists(String),

    /// A root already exists — a tree has exactly one root.
    #[error("root already exists: {0}")]
    RootAlreadyExists(String),

    /// [`crate::tree::SessionTree::remove`] refused: the node has children, so
    /// removing it would orphan them (dangling edges). Use
    /// [`crate::tree::SessionTree::remove_subtree`] for an explicit cascade.
    #[error("remove refused: node {0} has children (would orphan them); use remove_subtree")]
    WouldOrphan(String),

    /// [`crate::tree::SessionTree::reparent`] refused: the move would create a
    /// cycle — the new parent is the child itself (a self-loop) or a
    /// descendant of it — violating the acyclic-tree invariant.
    #[error("reparent refused: would create a cycle: {0}")]
    WouldCycle(String),

    /// [`crate::tree::SessionTree::reparent`] refused: the root has no parent
    /// by definition, so it cannot be moved (any other existing node is its
    /// descendant — the cycle case).
    #[error("cannot reparent the root: {0}")]
    CannotReparentRoot(String),

    /// A persisted snapshot failed [`crate::store::TreeSnapshot::validate`].
    #[error("invalid snapshot: {0}")]
    InvalidSnapshot(String),

    /// The store backend failed. The in-memory store never produces this; it is
    /// reserved for future store ports (e.g. SQLite).
    #[error("store failure: {0}")]
    Store(String),
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, TreeError>;
