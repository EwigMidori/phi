//! Neutral kernel errors (not product `ProjectError`).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("generation commit failed: {0}")]
    CommitFailed(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("session is not live: {0}")]
    SessionNotLive(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, KernelError>;
