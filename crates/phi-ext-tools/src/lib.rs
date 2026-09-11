#![forbid(unsafe_code)]
//! Host-bound tools. No product paths, downloads, or permission policy.

mod execution;
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
