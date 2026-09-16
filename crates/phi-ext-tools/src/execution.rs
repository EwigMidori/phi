use crate::{
    ProcessRequest, ProcessSupervisor, ProcessTermination, ToolExecution, ToolExecutor,
    worker_protocol::{JavaScriptRequest, JavaScriptResponse},
};
use async_trait::async_trait;
use phi_kernel::{ToolName, ToolResultStatus, ToolSpec, TurnCancel};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct ExecutionLimits {
    pub default_timeout: Duration,
    pub max_timeout: Duration,
    pub code_bytes: usize,
    pub output_bytes: usize,
    pub result_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeInput {
    code: String,
    timeout_ms: Option<u64>,
}
impl CodeInput {
    fn parse(input: Value, limits: &ExecutionLimits) -> Result<(Self, Duration), ToolExecution> {
        let input: Self = serde_json::from_value(input)
            .map_err(|e| ToolExecution::error("InvalidArguments", e.to_string()))?;
        if input.code.is_empty() || input.code.len() > limits.code_bytes {
            return Err(ToolExecution::error(
                "InvalidArguments",
                format!("code must contain 1..={} UTF-8 bytes", limits.code_bytes),
            ));
        }
        let timeout = input
            .timeout_ms
            .map_or(limits.default_timeout, Duration::from_millis);
        if timeout.is_zero() || timeout > limits.max_timeout {
            return Err(ToolExecution::error(
                "InvalidArguments",
                format!(
                    "timeout_ms must be positive and <= {}",
                    limits.max_timeout.as_millis()
                ),
            ));
        }
        Ok((input, timeout))
    }
}

struct CodeSpec;
impl CodeSpec {
    fn build(name: &str, description: &str) -> ToolSpec {
        ToolSpec {
            name: ToolName::from(name),
            description: description.into(),
            parameters: Some(
                json!({"type":"object","properties":{"code":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1,"description":"Optional execution timeout in milliseconds"}},"required":["code"],"additionalProperties":false}),
            ),
        }
    }
    fn directory(root: &PathBuf) -> Result<tempfile::TempDir, ToolExecution> {
        std::fs::create_dir_all(root)
            .map_err(|e| ToolExecution::error("WorkspaceUnavailable", e.to_string()))?;
        tempfile::Builder::new()
            .prefix("execution-")
            .tempdir_in(root)
            .map_err(|e| ToolExecution::error("WorkspaceUnavailable", e.to_string()))
    }
    fn outcome(result: &crate::ProcessOutcome) -> ToolExecution {
        let (status, error) = match result.termination {
            ProcessTermination::Cancelled => (
                ToolResultStatus::Incomplete,
                Some(json!({"kind":"Cancelled","message":"Execution cancelled and reaped"})),
            ),
            ProcessTermination::TimedOut => (
                ToolResultStatus::Error,
                Some(json!({"kind":"Timeout","message":"Execution exceeded its deadline"})),
            ),
            ProcessTermination::Exited if !result.success => (
                ToolResultStatus::Error,
                Some(
                    json!({"kind":"ProcessError","message":"Interpreter returned a nonzero exit status"}),
                ),
            ),
            ProcessTermination::Exited => (ToolResultStatus::Ok, None),
        };
        ToolExecution {
            status,
            output: json!({"stdout":result.stdout, "stderr":result.stderr, "elapsedMs":result.elapsed.as_millis(), "exitCode":result.exit_code,"error":error}),
        }
    }
}

pub struct JavaScriptExecutor {
    worker: PathBuf,
    runs: PathBuf,
    limits: ExecutionLimits,
    supervisor: ProcessSupervisor,
}
impl JavaScriptExecutor {
    pub fn tool_spec() -> ToolSpec {
        CodeSpec::build(
            "eval_js",
            "Evaluate synchronous JavaScript for calculations. Fresh globals on every call. No Node, DOM, imports, or asynchronous tasks. console.log/error produce output; the script completion value is returned. BigInt is returned as an exact decimal string with a type marker.",
        )
    }

    pub fn request(
        input: Value,
        limits: &ExecutionLimits,
    ) -> Result<JavaScriptRequest, ToolExecution> {
        let (input, timeout) = CodeInput::parse(input, limits)?;
        Ok(JavaScriptRequest {
            code: input.code,
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            output_limit: limits.output_bytes,
            result_limit: limits.result_bytes,
        })
    }

    pub fn new(worker: PathBuf, runs: PathBuf, limits: ExecutionLimits) -> Self {
        Self {
            worker,
            runs,
            limits,
            supervisor: ProcessSupervisor,
        }
    }
}
#[async_trait]
impl ToolExecutor for JavaScriptExecutor {
    fn spec(&self) -> ToolSpec {
        Self::tool_spec()
    }
    async fn execute(&self, input: Value, cancel: TurnCancel) -> ToolExecution {
        let (input, timeout) = match CodeInput::parse(input, &self.limits) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let directory = match CodeSpec::directory(&self.runs) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let mut request = ProcessRequest::new(
            self.worker.clone(),
            directory.path().to_path_buf(),
            timeout,
            self.limits
                .output_bytes
                .saturating_mul(16)
                .saturating_add(self.limits.result_bytes.saturating_mul(8))
                .saturating_add(4096),
        );
        request.stdin = match serde_json::to_vec(&JavaScriptRequest {
            code: input.code,
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            output_limit: self.limits.output_bytes,
            result_limit: self.limits.result_bytes,
        }) {
            Ok(value) => value,
            Err(error) => return ToolExecution::error("WorkerProtocol", error.to_string()),
        };
        let result = match self.supervisor.run(request, &cancel).await {
            Ok(value) => value,
            Err(error) => return ToolExecution::error("WorkerUnavailable", error.to_string()),
        };
        let mut execution = CodeSpec::outcome(&result);
        if result.termination != ProcessTermination::Exited || !result.success {
            return execution;
        }
        let response: JavaScriptResponse = match serde_json::from_str(&result.stdout.text) {
            Ok(value) if !result.stdout.truncated => value,
            Ok(_) => {
                return ToolExecution::error(
                    "WorkerProtocol",
                    "Worker response exceeded its bound",
                );
            }
            Err(error) => return ToolExecution::error("WorkerProtocol", error.to_string()),
        };
        execution.status = if response.error.is_some() {
            ToolResultStatus::Error
        } else {
            ToolResultStatus::Ok
        };
        execution.output = json!({"stdout":response.stdout,"stderr":response.stderr,"value":response.value,"error":response.error,"elapsedMs":result.elapsed.as_millis(),"exitCode":result.exit_code});
        execution
    }
}

/// Keeps a host-owned environment lease alive through process reaping.
pub struct PythonLease {
    interpreter: PathBuf,
    _guard: Box<dyn Send + Sync>,
}
impl PythonLease {
    pub fn new(interpreter: PathBuf, guard: impl Send + Sync + 'static) -> Self {
        Self {
            interpreter,
            _guard: Box::new(guard),
        }
    }
    pub fn interpreter(&self) -> &std::path::Path {
        &self.interpreter
    }
}

#[async_trait]
pub trait PythonEnvironment: Send + Sync {
    async fn acquire(&self, cancel: &TurnCancel) -> Result<PythonLease, ToolExecution>;
}

pub struct PythonExecutor {
    environment: Arc<dyn PythonEnvironment>,
    runs: PathBuf,
    limits: ExecutionLimits,
    supervisor: ProcessSupervisor,
}
impl PythonExecutor {
    pub fn new(
        environment: Arc<dyn PythonEnvironment>,
        runs: PathBuf,
        limits: ExecutionLimits,
    ) -> Self {
        Self {
            environment,
            runs,
            limits,
            supervisor: ProcessSupervisor,
        }
    }
}
#[async_trait]
impl ToolExecutor for PythonExecutor {
    fn spec(&self) -> ToolSpec {
        CodeSpec::build(
            "run_python",
            "Execute Python calculations in the application's dedicated virtual environment. Fresh process and temporary working directory each call; variables and files do not persist. Use print() for results; the last expression is not automatically printed. Standard library and pip are available. Install packages with subprocess.run([sys.executable, '-m', 'pip', 'install', 'package'], check=True); installed packages persist. The execution deadline includes package installation.",
        )
    }
    async fn execute(&self, input: Value, cancel: TurnCancel) -> ToolExecution {
        let (input, timeout) = match CodeInput::parse(input, &self.limits) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let lease = match self.environment.acquire(&cancel).await {
            Ok(value) => value,
            Err(error) => return error,
        };
        if cancel.is_cancelled() {
            return ToolExecution::cancelled();
        }
        let directory = match CodeSpec::directory(&self.runs) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let source = directory.path().join("calculation.py");
        if let Err(error) = std::fs::write(&source, input.code) {
            return ToolExecution::error("WorkspaceUnavailable", error.to_string());
        }
        let mut request = ProcessRequest::new(
            lease.interpreter().to_path_buf(),
            directory.path().to_path_buf(),
            timeout,
            self.limits.output_bytes,
        );
        request.arguments = vec![
            "-I".into(),
            "-X".into(),
            "utf8".into(),
            "-u".into(),
            source.into_os_string(),
        ];
        request.remove_environment = [
            "PYTHONHOME",
            "PYTHONPATH",
            "PIP_TARGET",
            "PIP_PREFIX",
            "PIP_USER",
            "PIP_PYTHON",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        request
            .environment
            .insert("PIP_REQUIRE_VIRTUALENV".into(), "true".into());
        request.environment.insert(
            "VIRTUAL_ENV".into(),
            lease
                .interpreter()
                .parent()
                .and_then(std::path::Path::parent)
                .expect("venv interpreter")
                .as_os_str()
                .to_owned(),
        );
        match self.supervisor.run(request, &cancel).await {
            Ok(result) => CodeSpec::outcome(&result),
            Err(error) => ToolExecution::error("InterpreterUnavailable", error.to_string()),
        }
    }
}
