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
            "Evaluate synchronous JavaScript for calculations. Fresh globals on every call. No Node, DOM, imports, or asynchronous tasks. console.log/error/warn inspect objects and arrays into readable output (up to 8 levels deep); the script completion value is returned separately. BigInt is returned as an exact decimal string with a type marker.",
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
    artifacts: Option<Arc<dyn crate::ExecutionArtifacts>>,
    environment: Arc<dyn PythonEnvironment>,
    runs: PathBuf,
    limits: ExecutionLimits,
    supervisor: ProcessSupervisor,
}
impl PythonExecutor {
    pub fn with_artifacts(mut self, artifacts: Arc<dyn crate::ExecutionArtifacts>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }
    pub fn new(
        environment: Arc<dyn PythonEnvironment>,
        runs: PathBuf,
        limits: ExecutionLimits,
    ) -> Self {
        Self {
            artifacts: None,
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
        let mut spec = CodeSpec::build(
            "run_python",
            "Execute Python in a fresh process and temporary directory. print() returns text. Use artifacts.publish('plot.png') or artifacts.publish('data.csv') to persist files. artifacts is provided by this tool: use it directly, import artifacts, or from artifacts import publish; do not pip install artifacts. Published files are collected only on successful exit, maximum 16 files and 32 MiB total. Matplotlib uses Agg: savefig(), then publish(); do not use show(). Files from earlier results can be staged using inputs:[{id: artifact ID, path: relative filename}]. Variables do not persist. pip packages persist; install with subprocess.run([sys.executable, '-m', 'pip', 'install', 'package'], check=True). Return useful numeric summaries with print even when publishing a plot.",
        );
        spec.parameters.as_mut().expect("code schema")["properties"]["inputs"] = json!({"type":"array","maxItems":16,"items":{"type":"object","properties":{"id":{"type":"string"},"path":{"type":"string"}},"required":["id","path"],"additionalProperties":false}});
        spec
    }

    async fn execute(&self, input: Value, cancel: TurnCancel) -> ToolExecution {
        self.run(None, input, cancel).await
    }
    async fn execute_in(
        &self,
        session: &phi_kernel::SessionId,
        input: Value,
        cancel: TurnCancel,
    ) -> ToolExecution {
        self.run(Some(session), input, cancel).await
    }
}
impl PythonExecutor {
    async fn run(
        &self,
        session: Option<&phi_kernel::SessionId>,
        mut input: Value,
        cancel: TurnCancel,
    ) -> ToolExecution {
        let inputs: Vec<crate::artifacts::ArtifactInput> =
            match input.as_object_mut().and_then(|v| v.remove("inputs")) {
                Some(value) => match serde_json::from_value(value) {
                    Ok(v) => v,
                    Err(e) => return ToolExecution::error("InvalidArguments", e.to_string()),
                },
                None => Vec::new(),
            };
        if inputs.len() > crate::MAX_ARTIFACTS {
            return ToolExecution::error("InvalidArguments", "At most 16 input artifacts");
        }
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
        let mut input_bytes = 0usize;
        for artifact in inputs {
            let (Some(store), Some(session)) = (&self.artifacts, session) else {
                return ToolExecution::error(
                    "ArtifactsUnavailable",
                    "No artifact store bound to this session",
                );
            };
            let path = match crate::artifacts::ArtifactWorkspace::relative(&artifact.path) {
                Ok(path)
                    if !matches!(
                        path.to_str(),
                        Some("calculation.py" | "bootstrap.py" | ".artifacts.json")
                    ) =>
                {
                    directory.path().join(path)
                }
                _ => {
                    return ToolExecution::error(
                        "InvalidArguments",
                        "Invalid or reserved input path",
                    );
                }
            };
            let bytes = match store.read(session, &artifact.id).await {
                Ok(v) => v,
                Err(e) => return ToolExecution::error("ArtifactRead", e),
            };
            input_bytes = input_bytes.saturating_add(bytes.len());
            if input_bytes > crate::MAX_ARTIFACT_BYTES {
                return ToolExecution::error("ArtifactRead", "Input artifacts exceed 32 MiB");
            }
            if cancel.is_cancelled() {
                return ToolExecution::cancelled();
            }
            let written = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(path.parent().expect("input parent"))?;
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)?
                    .write_all(&bytes)
            })();
            if let Err(e) = written {
                return ToolExecution::error("ArtifactRead", e.to_string());
            }
        }
        if let Err(e) = std::fs::write(
            directory.path().join("bootstrap.py"),
            include_str!("python_bootstrap.py"),
        ) {
            return ToolExecution::error("WorkspaceUnavailable", e.to_string());
        }
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
            directory.path().join("bootstrap.py").into_os_string(),
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
            Ok(result) => {
                let mut execution = CodeSpec::outcome(&result);
                if execution.status != ToolResultStatus::Ok {
                    return execution;
                }
                if cancel.is_cancelled() {
                    return ToolExecution::cancelled();
                }
                let published = async {
                    let files = crate::artifacts::ArtifactWorkspace::collect(directory.path())?;
                    if files.is_empty() {
                        return Ok(Vec::new());
                    }
                    let (Some(store), Some(session)) = (&self.artifacts, session) else {
                        return Err("No artifact store bound to this session".to_string());
                    };
                    store.publish(session, files).await
                }
                .await;
                match published {
                    Ok(artifacts) => execution.output["artifacts"] = json!(artifacts),
                    Err(error) => {
                        execution.status = ToolResultStatus::Error;
                        execution.output["error"] =
                            json!({"kind":"ArtifactPublish", "message":error});
                    }
                }
                execution
            }
            Err(error) => ToolExecution::error("InterpreterUnavailable", error.to_string()),
        }
    }
}
