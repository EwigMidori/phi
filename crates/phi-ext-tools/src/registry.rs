use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use phi_kernel::{ToolArguments, ToolName, ToolResultStatus, ToolSpec, TurnCancel};
use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub status: ToolResultStatus,
    pub output: Value,
}

impl ToolExecution {
    pub fn error(kind: &str, message: impl Into<String>) -> Self {
        Self {
            status: ToolResultStatus::Error,
            output: json!({"error": {"kind":kind, "message":message.into()}}),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            status: ToolResultStatus::Incomplete,
            output: json!({"error":{"kind":"Cancelled","message":"Execution cancelled and reaped"}}),
        }
    }
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn spec(&self) -> ToolSpec;
    /// Shared by durable call recording and execution; must be idempotent.
    fn normalize_input(&self, input: Value) -> Result<Value, ToolExecution> {
        Ok(input)
    }
    /// Must return only after owned resources have stopped, including cancellation.
    async fn execute(&self, input: Value, cancel: TurnCancel) -> ToolExecution;
    async fn execute_in(
        &self,
        _session: &phi_kernel::SessionId,
        input: Value,
        cancel: TurnCancel,
    ) -> ToolExecution {
        self.execute(input, cancel).await
    }
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn ToolExecutor>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn ToolExecutor>) -> Result<(), String> {
        let spec = tool.spec();
        if self
            .tools
            .iter()
            .any(|known| known.spec().name == spec.name)
        {
            return Err(format!("Duplicate tool binding: {}", spec.name.as_str()));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec()).collect()
    }

    pub fn normalize_arguments(
        &self,
        name: &ToolName,
        arguments: &ToolArguments,
    ) -> Result<ToolArguments, ToolExecution> {
        let input = self.prepare_input(name, arguments.as_str())?;
        Ok(ToolArguments::new(input.to_string()))
    }

    pub fn scope(self: &Arc<Self>, cancel: TurnCancel) -> ToolExecutionScope {
        ToolExecutionScope {
            registry: self.clone(),
            session: None,
            cancel,
            closed: AtomicBool::new(false),
            joined: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn scope_for(
        self: &Arc<Self>,
        session: phi_kernel::SessionId,
        cancel: TurnCancel,
    ) -> ToolExecutionScope {
        let mut scope = self.scope(cancel);
        scope.session = Some(session);
        scope
    }

    async fn execute(
        &self,
        session: Option<&phi_kernel::SessionId>,
        name: &ToolName,
        arguments: &str,
        cancel: TurnCancel,
    ) -> ToolExecution {
        if cancel.is_cancelled() {
            return ToolExecution::cancelled();
        }
        let input = match self.prepare_input(name, arguments) {
            Ok(input) => input,
            Err(error) => return error,
        };
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.spec().name == *name)
            .expect("prepared tool binding");
        match session {
            Some(session) => tool.execute_in(session, input, cancel).await,
            None => tool.execute(input, cancel).await,
        }
    }

    fn prepare_input(&self, name: &ToolName, arguments: &str) -> Result<Value, ToolExecution> {
        if arguments.len() > 2 * 1024 * 1024 {
            return Err(ToolExecution::error(
                "InvalidArguments",
                "Tool arguments exceed 2 MiB",
            ));
        }
        let Some(tool) = self.tools.iter().find(|tool| tool.spec().name == *name) else {
            return Err(ToolExecution::error(
                "UnknownTool",
                format!("No executable binding for {}", name.as_str()),
            ));
        };
        let input = serde_json::from_str(arguments)
            .map_err(|error| ToolExecution::error("InvalidArguments", error.to_string()))?;
        tool.normalize_input(input)
    }
}

/// A single generation's owned executions. Dropping the stream does not lose child ownership.
pub struct ToolExecutionScope {
    session: Option<phi_kernel::SessionId>,
    registry: Arc<ToolRegistry>,
    cancel: TurnCancel,
    closed: AtomicBool,
    joined: AtomicBool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl ToolExecutionScope {
    pub async fn execute(&self, name: &ToolName, arguments: &str) -> ToolExecution {
        if self.closed.load(Ordering::SeqCst) || self.cancel.is_cancelled() {
            return ToolExecution::cancelled();
        }
        let receiver = {
            let mut tasks = self.tasks.lock().await;
            if self.closed.load(Ordering::SeqCst) || self.cancel.is_cancelled() {
                return ToolExecution::cancelled();
            }
            let (sender, receiver) = oneshot::channel();
            let session = self.session.clone();
            let (registry, name, arguments, cancel) = (
                self.registry.clone(),
                name.clone(),
                arguments.to_owned(),
                self.cancel.clone(),
            );
            tasks.push(tokio::spawn(async move {
                let result = registry
                    .execute(session.as_ref(), &name, &arguments, cancel)
                    .await;
                let _ = sender.send(result);
            }));
            receiver
        };
        receiver
            .await
            .unwrap_or_else(|error| ToolExecution::error("ExecutorFailed", error.to_string()))
    }

    pub async fn close_and_join(&self) {
        self.cancel.cancel();
        self.join().await;
    }

    /// Successful completion closes the scope without turning its generation into a cancellation.
    pub async fn join(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let mut tasks = self.tasks.lock().await;
        // Keep each handle owned until its await finishes. A cancelled close future
        // can be retried; it must not detach tasks by taking the whole collection.
        while let Some(task) = tasks.last_mut() {
            let _ = task.await;
            tasks.pop();
        }
        self.joined.store(true, Ordering::SeqCst);
    }
}

impl Drop for ToolExecutionScope {
    fn drop(&mut self) {
        if !self.joined.load(Ordering::SeqCst) {
            self.cancel.cancel();
        }
    }
}
