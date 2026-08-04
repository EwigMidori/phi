//! Crate-local errors for the session tree.

use thiserror::Error;

/// Errors from [`crate::tree::SessionTree`] operations.
///
/// This crate keeps its own error type instead of reusing the kernel's
/// [`phi_kernel::KernelError`]: the tree contract is about topology and
/// lifecycle, not the kernel's generation queue. The shape mirrors kernel
/// conventions (not-found / not-live style messages).
#[derive(Debug, Error)]
pub enum TreeError {
    /// The session id is not a node in the tree.
    #[error("node not found: {0}")]
    NodeNotFound(String),

    /// The node exists but is not [`crate::model::NodeState::Live`] (e.g.
    /// deriving from a Tombstoned node).
    #[error("node is not live: {0}")]
    NodeNotLive(String),

    /// The id is already a node (duplicate [`crate::tree::SessionTree::add_root`]).
    #[error("node already exists: {0}")]
    NodeAlreadyExists(String),

    /// A root already exists — a tree has exactly one root.
    #[error("root already exists: {0}")]
    RootAlreadyExists(String),

    /// A persisted snapshot failed [`crate::store::TreeSnapshot::validate`].
    #[error("invalid snapshot: {0}")]
    InvalidSnapshot(String),

    /// The injected close policy asked for a mutation that would break tree
    /// invariants (hard remove of a non-leaf would orphan its children).
    #[error("close refused: {0}")]
    CloseRefused(String),

    /// The store backend failed. The in-memory store never produces this; it is
    /// reserved for future store ports (e.g. SQLite).
    #[error("store failure: {0}")]
    Store(String),
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, TreeError>;
