use async_trait::async_trait;
use phi_ext_tools::{ToolExecution, ToolExecutor, ToolRegistry};
use phi_kernel::{ToolName, ToolResultStatus, ToolSpec, TurnCancel};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct WaitingTool {
    started: Arc<tokio::sync::Notify>,
    reaped: Arc<AtomicBool>,
}
#[async_trait]
impl ToolExecutor for WaitingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::from("waiting"),
            description: "fixture".into(),
            parameters: None,
        }
    }
    async fn execute(&self, _: Value, cancel: TurnCancel) -> ToolExecution {
        self.started.notify_one();
        cancel.cancelled().await;
        tokio::task::yield_now().await;
        self.reaped.store(true, Ordering::SeqCst);
        ToolExecution::cancelled()
    }
}

#[tokio::test]
async fn losing_the_stream_waiter_does_not_lose_execution_ownership() {
    let started = Arc::new(tokio::sync::Notify::new());
    let reaped = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(WaitingTool {
            started: started.clone(),
            reaped: reaped.clone(),
        }))
        .unwrap();
    let registry = Arc::new(registry);
    let scope = Arc::new(registry.scope(TurnCancel::new()));
    let running = scope.clone();
    let waiter =
        tokio::spawn(async move { running.execute(&ToolName::from("waiting"), "{}").await });
    started.notified().await;
    waiter.abort();
    let _ = waiter.await;
    scope.close_and_join().await;
    assert!(reaped.load(Ordering::SeqCst));
    assert_eq!(
        scope.execute(&ToolName::from("waiting"), "{}").await.status,
        ToolResultStatus::Incomplete
    );
}

#[tokio::test]
async fn invalid_arguments_and_unknown_bindings_never_start_an_executor() {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(WaitingTool {
            started: Arc::new(tokio::sync::Notify::new()),
            reaped: Arc::new(AtomicBool::new(false)),
        }))
        .unwrap();
    let registry = Arc::new(registry);
    let cancel = TurnCancel::new();
    let scope = registry.scope(cancel.clone());
    assert_eq!(
        scope.execute(&ToolName::from("missing"), "{}").await.status,
        ToolResultStatus::Error
    );
    assert_eq!(
        scope
            .execute(&ToolName::from("waiting"), "{malformed")
            .await
            .status,
        ToolResultStatus::Error
    );
    scope.join().await;
    drop(scope);
    assert!(!cancel.is_cancelled());
}
