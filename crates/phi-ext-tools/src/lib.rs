#![forbid(unsafe_code)]
//! Host-bound tools. No product paths, downloads, or permission policy.

mod artifacts;
mod execution;
pub use artifacts::{
    ArtifactFile, ArtifactId, ArtifactMetadata, ExecutionArtifacts, MAX_ARTIFACT_BYTES,
    MAX_ARTIFACTS,
};
mod process;
mod registry;
pub mod worker_protocol;

pub use execution::{
    ExecutionLimits, JavaScriptExecutor, PythonEnvironment, PythonExecutor, PythonLease,
};
pub use process::{
    CapturedOutput, ProcessOutcome, ProcessRequest, ProcessSupervisor, ProcessTermination,
};
pub use registry::{ToolExecution, ToolExecutionScope, ToolExecutor, ToolRegistry};
