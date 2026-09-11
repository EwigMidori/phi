use super::*;
use async_trait::async_trait;
use futures::stream;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn ports(agent: Arc<dyn AgentRuntime>) -> AgentPorts {
    AgentPorts::from_sources(
        agent,
        Arc::new(EmptyAgentPrefix),
        Arc::new(FixedToolCallSeal(ToolCallSealPolicy::SealAlways)),
    )
}
fn setup() -> (InMemoryTranscript, SessionId) {
    let store = InMemoryTranscript::new();
    let sid = SessionId::generate();
    store.ensure_live(&sid).unwrap();
    (store, sid)
}
fn user(store: &dyn Transcript, sid: &SessionId, text: &str) -> GenerationJob {
    let row = store.record_user(sid, &MessageContent::text(text)).unwrap();
    GenerationJob::new(sid.clone(), row.message_id)
}
fn response(text: &str) -> ModelResponse {
    ModelResponse {
        id: ModelResponseId::generate(),
        rows: vec![TranscriptRow::new(
            MessageId::generate(),
            TurnItem::Assistant {
                content: text.into(),
            },
        )],
        continuation: None,
        complete: true,
    }
}
fn tool_response() -> ModelResponse {
    ModelResponse {
        id: ModelResponseId::generate(),
        rows: vec![
            TranscriptRow::new(
                MessageId::generate(),
                TurnItem::Assistant {
                    content: "before".into(),
                },
            ),
            TranscriptRow::new(
                MessageId::generate(),
                TurnItem::ToolCall {
                    tool_call_id: ToolCallId::new("compute"),
                    tool_name: ToolName::new("calculate"),
                    input: ToolArguments::new("{bad json"),
                },
            ),
        ],
        continuation: None,
        complete: true,
    }
}

struct CaptureAgent(Arc<Mutex<Vec<Vec<TurnItem>>>>);
#[async_trait]
impl AgentRuntime for CaptureAgent {
    async fn run(&self, request: TurnRequest) -> std::result::Result<AgentRun, String> {
        self.0.lock().unwrap().push(request.history);
        Ok(AgentRun::text("answer"))
    }
}

#[tokio::test]
async fn queued_inputs_follow_causal_history_and_truncate_logically() {
    let (store, sid) = setup();
    let first = user(&store, &sid, "one");
    let second = user(&store, &sid, "two");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let directory = SessionDirectory::new(ports(Arc::new(CaptureAgent(captured.clone()))));
    directory.enqueue(&sid, first).unwrap();
    directory.enqueue(&sid, second.clone()).unwrap();
    let (bus, _) = tokio::sync::broadcast::channel(32);
    directory.run_until_idle(&sid, &store, &bus).await.unwrap();
    let captures = captured.lock().unwrap();
    assert_eq!(captures[0].len(), 1);
    assert_eq!(captures[1].len(), 3);
    assert!(
        matches!(&captures[1][1],TurnItem::ModelResponse{response} if matches!(&response.rows[0].item,TurnItem::Assistant{content} if content=="answer"))
    );
    let rows = store.load_rows(&sid).unwrap();
    assert!(matches!(rows[1].item, TurnItem::Assistant { .. }));
    assert_eq!(rows[2].id, second.user_message_id);
    store.truncate_from(&sid, &second.user_message_id).unwrap();
    assert_eq!(store.load_rows(&sid).unwrap().len(), 2);
}

#[test]
fn response_batch_rejects_duplicate_calls_without_partial_mutation() {
    let (store, sid) = setup();
    let job = user(&store, &sid, "calculate");
    store
        .commit_generation(&GenerationCommit::Start { job: job.clone() })
        .unwrap();
    let mut batch = tool_response();
    let mut duplicate = batch.rows[1].clone();
    duplicate.id = MessageId::generate();
    batch.rows.push(duplicate);
    assert!(
        store
            .commit_generation(&GenerationCommit::Response {
                job,
                response: batch
            })
            .is_err()
    );
    assert_eq!(store.load_rows(&sid).unwrap().len(), 1);
}

#[test]
fn raw_arguments_and_continuation_survive_detached_snapshot() {
    let (store, sid) = setup();
    let job = user(&store, &sid, "calculate");
    store
        .commit_generation(&GenerationCommit::Start { job: job.clone() })
        .unwrap();
    let mut batch = tool_response();
    let opaque = ProviderContinuation {
        scope: "provider/model".into(),
        payload: serde_json::json!([{"opaque":"data"}]),
    };
    batch.continuation = Some(opaque.clone());
    store
        .commit_generation(&GenerationCommit::Response {
            job: job.clone(),
            response: batch,
        })
        .unwrap();
    let detached = store.detached();
    let loaded = detached.load_job_history(&job).unwrap();
    let TurnItem::ModelResponse { response } = &loaded[1] else {
        panic!("group expected")
    };
    assert_eq!(response.continuation, Some(opaque));
    assert!(
        matches!(&response.rows[1].item,TurnItem::ToolCall{input,..} if input.as_str()=="{bad json")
    );
}

#[test]
fn interrupted_response_remains_incomplete_after_reopen() {
    let (store, sid) = setup();
    let job = user(&store, &sid, "calculate");
    store
        .commit_generation(&GenerationCommit::Start { job: job.clone() })
        .unwrap();
    let mut partial = response("partial answer");
    partial.complete = false;
    store
        .commit_generation(&GenerationCommit::Response {
            job: job.clone(),
            response: partial,
        })
        .unwrap();
    store
        .commit_generation(&GenerationCommit::Finish {
            job: job.clone(),
            status: GenerationStatus::Stopped,
        })
        .unwrap();
    let reopened = InMemoryTranscript::new();
    reopened
        .replace_session(&sid, store.session_snapshot(&sid).unwrap())
        .unwrap();
    assert!(
        matches!(&reopened.load_job_history(&job).unwrap()[1],TurnItem::ModelResponse{response} if !response.complete)
    );
}

#[test]
fn snapshot_rejects_corrupt_tool_pairing_and_response_membership() {
    let (store, sid) = setup();
    let job = user(&store, &sid, "calculate");
    store
        .commit_generation(&GenerationCommit::Start { job: job.clone() })
        .unwrap();
    let batch = tool_response();
    let response_id = batch.id.clone();
    store
        .commit_generation(&GenerationCommit::Response {
            job: job.clone(),
            response: batch,
        })
        .unwrap();
    store
        .commit_generation(&GenerationCommit::ToolResult {
            job,
            response_id,
            tool_call_id: ToolCallId::new("compute"),
            tool_name: ToolName::new("calculate"),
            output: serde_json::json!(3),
            status: ToolResultStatus::Ok,
        })
        .unwrap();
    let valid = store.session_snapshot(&sid).unwrap();
    let mut orphan = valid.clone();
    orphan.rows.remove(2);
    assert!(orphan.validate().is_err());
    let mut duplicate = valid.clone();
    let mut row = duplicate.rows[3].clone();
    row.id = MessageId::generate();
    duplicate.rows.push(row);
    assert!(duplicate.validate().is_err());
    let mut mismatched = valid.clone();
    if let TurnItem::ToolResult { tool_name, .. } = &mut mismatched.rows[3].item {
        *tool_name = ToolName::new("other");
    }
    assert!(mismatched.validate().is_err());
    let mut partial = valid;
    partial.rows[2]
        .generation
        .as_mut()
        .unwrap()
        .response_complete = false;
    assert!(partial.validate().is_err());
}

struct FailResponse(InMemoryTranscript);
impl Transcript for FailResponse {
    fn commit_generation(&self, commit: &GenerationCommit) -> Result<u64> {
        if matches!(commit, GenerationCommit::Response { .. }) {
            Err(KernelError::CommitFailed("disk full".into()))
        } else {
            self.0.commit_generation(commit)
        }
    }
    fn session_snapshot(&self, sid: &SessionId) -> Result<TranscriptSession> {
        self.0.session_snapshot(sid)
    }
    fn ensure_live(&self, sid: &SessionId) -> Result<()> {
        self.0.ensure_live(sid)
    }
    fn is_live(&self, sid: &SessionId) -> Result<bool> {
        self.0.is_live(sid)
    }
    fn version(&self) -> Result<u64> {
        self.0.version()
    }
    fn load_rows(&self, sid: &SessionId) -> Result<Vec<TranscriptRow>> {
        self.0.load_rows(sid)
    }
    fn record_user(&self, sid: &SessionId, content: &MessageContent) -> Result<RecordResult> {
        self.0.record_user(sid, content)
    }
    fn truncate_from(&self, sid: &SessionId, id: &MessageId) -> Result<TruncateResult> {
        self.0.truncate_from(sid, id)
    }
    fn record_assistant(
        &self,
        sid: &SessionId,
        id: &MessageId,
        text: &str,
    ) -> Result<RecordResult> {
        self.0.record_assistant(sid, id, text)
    }
    fn record_reasoning(&self, sid: &SessionId, text: &str) -> Result<RecordResult> {
        self.0.record_reasoning(sid, text)
    }
    fn record_tool_call(
        &self,
        sid: &SessionId,
        id: &ToolCallId,
        name: &ToolName,
        input: &ToolArguments,
    ) -> Result<RecordResult> {
        self.0.record_tool_call(sid, id, name, input)
    }
    fn record_tool_result(
        &self,
        sid: &SessionId,
        id: &ToolCallId,
        name: &ToolName,
        output: &serde_json::Value,
        status: ToolResultStatus,
    ) -> Result<RecordResult> {
        self.0.record_tool_result(sid, id, name, output, status)
    }
}
struct PullToolAgent {
    executed: Arc<AtomicUsize>,
    reaped: Arc<AtomicBool>,
    started: Option<Arc<tokio::sync::Notify>>,
}
struct Lifecycle(Arc<AtomicBool>);
#[async_trait]
impl AgentRunLifecycle for Lifecycle {
    async fn close_and_join(&self) -> std::result::Result<(), String> {
        tokio::task::yield_now().await;
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait]
impl AgentRuntime for PullToolAgent {
    async fn run(&self, _: TurnRequest) -> std::result::Result<AgentRun, String> {
        let executed = self.executed.clone();
        let started = self.started.clone();
        let batch = tool_response();
        let response_id = batch.id.clone();
        let stream = stream::unfold((0, batch), move |(phase, batch)| {
            let executed = executed.clone();
            let started = started.clone();
            let response_id = response_id.clone();
            async move {
                let event = match phase {
                    0 => AgentEvent::ModelResponseCompleted {
                        response: batch.clone(),
                    },
                    1 => {
                        executed.fetch_add(1, Ordering::SeqCst);
                        if let Some(started) = started {
                            started.notify_one();
                            futures::future::pending::<()>().await;
                        }
                        AgentEvent::ToolResult {
                            response_id,
                            tool_call_id: ToolCallId::new("compute"),
                            output: serde_json::json!(3),
                            status: ToolResultStatus::Ok,
                        }
                    }
                    2 => AgentEvent::ModelResponseCompleted {
                        response: response("after"),
                    },
                    3 => AgentEvent::Finished { reason: None },
                    _ => return None,
                };
                Some((Ok(event), (phase + 1, batch)))
            }
        });
        Ok(AgentRun::with_lifecycle(
            Box::pin(stream),
            Arc::new(Lifecycle(self.reaped.clone())),
        ))
    }
}

#[tokio::test]
async fn failed_response_commit_prevents_execution_and_joins_run() {
    let (store, sid) = setup();
    let job = user(&store, &sid, "calculate");
    let store = FailResponse(store);
    let executed = Arc::new(AtomicUsize::new(0));
    let reaped = Arc::new(AtomicBool::new(false));
    let directory = SessionDirectory::new(ports(Arc::new(PullToolAgent {
        executed: executed.clone(),
        reaped: reaped.clone(),
        started: None,
    })));
    directory.enqueue(&sid, job).unwrap();
    let (bus, _) = tokio::sync::broadcast::channel(32);
    assert!(directory.run_until_idle(&sid, &store, &bus).await.is_err());
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    assert!(reaped.load(Ordering::SeqCst));
    assert!(directory.is_idle(&sid));
}

#[tokio::test]
async fn tool_text_order_and_stop_join_before_incomplete_commit() {
    for stop in [false, true] {
        let (store, sid) = setup();
        let job = user(&store, &sid, "calculate");
        let started = Arc::new(tokio::sync::Notify::new());
        let reaped = Arc::new(AtomicBool::new(false));
        let directory = SessionDirectory::new(ports(Arc::new(PullToolAgent {
            executed: Arc::new(AtomicUsize::new(0)),
            reaped: reaped.clone(),
            started: stop.then(|| started.clone()),
        })));
        directory.enqueue(&sid, job).unwrap();
        let (bus, _) = tokio::sync::broadcast::channel(32);
        let pump = directory.run_until_idle(&sid, &store, &bus);
        let stopping = async {
            if stop {
                started.notified().await;
                directory.stop_and_wait(&sid).await.unwrap();
                assert!(reaped.load(Ordering::SeqCst));
            }
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(pump, stopping)
        })
        .await
        .unwrap();
        result.unwrap();
        let rows = store.load_rows(&sid).unwrap();
        assert!(matches!(&rows[1].item,TurnItem::Assistant{content} if content=="before"));
        assert!(matches!(rows[2].item, TurnItem::ToolCall { .. }));
        let expected = if stop {
            ToolResultStatus::Incomplete
        } else {
            ToolResultStatus::Ok
        };
        assert!(matches!(rows[3].item,TurnItem::ToolResult{status,..} if status==expected));
        if !stop {
            assert!(matches!(&rows[4].item,TurnItem::Assistant{content} if content=="after"));
        } else {
            assert_eq!(rows.len(), 4);
        }
    }
}
