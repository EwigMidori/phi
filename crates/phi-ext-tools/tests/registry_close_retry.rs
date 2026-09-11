use async_trait::async_trait;
use phi_ext_tools::{ToolExecution, ToolExecutor, ToolRegistry};
use phi_kernel::{ToolName, ToolSpec, TurnCancel};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct DeferredCleanup {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    reaped: AtomicBool,
}
#[async_trait]
impl ToolExecutor for DeferredCleanup {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::from("cleanup"),
            description: "fixture".into(),
            parameters: None,
        }
    }
    async fn execute(&self, _: Value, cancel: TurnCancel) -> ToolExecution {
        self.started.notify_one();
        cancel.cancelled().await;
        self.release.notified().await;
        self.reaped.store(true, Ordering::SeqCst);
        ToolExecution::cancelled()
    }
}

#[tokio::test]
async fn interrupted_close_retains_handles_for_a_second_join() {
    let tool = Arc::new(DeferredCleanup {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        reaped: AtomicBool::new(false),
    });
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).unwrap();
    let registry = Arc::new(registry);
    let scope = Arc::new(registry.scope(TurnCancel::new()));
    let running = scope.clone();
    let operation =
        tokio::spawn(async move { running.execute(&ToolName::from("cleanup"), "{}").await });
    tool.started.notified().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), scope.close_and_join())
            .await
            .is_err()
    );
    assert!(!tool.reaped.load(Ordering::SeqCst));
    tool.release.notify_one();
    scope.close_and_join().await;
    assert!(tool.reaped.load(Ordering::SeqCst));
    operation.await.unwrap();
}
