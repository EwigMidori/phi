//! OpenAI-compatible protocol fixtures, verified against function-calling docs
//! 2026-09-11. Assertions exercise continuation and execution, not serde derives.
use super::*;
use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use phi_ext_tools::{ToolExecutor, ToolRegistry};
use phi_kernel::{
    AgentPorts, FixedAgentPrefix, FixedToolCallSeal, GenerationJob, InMemoryTranscript,
    SessionDirectory, ToolResultStatus, Transcript,
};
use std::sync::{Mutex, atomic::AtomicUsize};

fn sse(values: Vec<Value>, done: bool) -> String {
    let mut text = String::new();
    for value in values {
        text.push_str(&format!("data: {value}\n\n"));
    }
    if done {
        text.push_str("data: [DONE]\n\n");
    }
    text
}
fn chat_calls() -> String {
    sse(
        vec![
            json!({"choices":[{"delta":{"content":"计算前"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"first","type":"function","function":{"name":"compute","arguments":"{\"value\":"}},{"index":1,"id":"second","type":"function","function":{"name":"missing","arguments":"{}"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"2}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}}),
        ],
        true,
    )
}
fn responses_calls(duplicate: bool) -> String {
    let output = json!([
        {"type":"reasoning","id":"reasoning-item","summary":[{"type":"summary_text","text":"calculate"}],"encrypted_content":"opaque-value"},
        {"type":"message","id":"message-item","role":"assistant","content":[{"type":"output_text","text":"计算前","annotations":[]}]},
        {"type":"function_call","id":"function-item-a","call_id":"first","name":"compute","arguments":"{\"value\":2}","status":"completed"},
        {"type":"function_call","id":"function-item-b","call_id":if duplicate{"first"}else{"second"},"name":"missing","arguments":"{}","status":"completed"}
    ]);
    sse(
        vec![
            json!({"type":"response.output_text.delta","delta":"计算前"}),
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"function-item-a","call_id":"first","name":"compute","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"item_id":"function-item-a","delta":"{\"value\":"}),
            json!({"type":"response.function_call_arguments.done","output_index":2,"item_id":"function-item-a","arguments":"{\"value\":2}"}),
            json!({"type":"response.completed","response":{"output":output,"usage":{"input_tokens":12,"output_tokens":4,"total_tokens":16}}}),
        ],
        false,
    )
}
fn final_response(style: ApiStyle) -> String {
    match style {
        ApiStyle::Completions => sse(
            vec![
                json!({"choices":[{"delta":{"content":"结果是 2"}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            ],
            true,
        ),
        ApiStyle::Responses => sse(
            vec![
                json!({"type":"response.output_text.delta","delta":"结果是 2"}),
                json!({"type":"response.completed","response":{"output":[{"type":"message","id":"final","role":"assistant","content":[{"type":"output_text","text":"结果是 2","annotations":[]}]}]}}),
            ],
            false,
        ),
    }
}

struct Compute(Arc<AtomicUsize>);
#[async_trait]
impl ToolExecutor for Compute {
    fn spec(&self) -> phi_kernel::ToolSpec {
        phi_kernel::ToolSpec {
            name: ToolName::new("compute"),
            description: "calculates".into(),
            parameters: Some(
                json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}),
            ),
        }
    }
    async fn execute(&self, input: Value, _: TurnCancel) -> ToolExecution {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolExecution {
            status: ToolResultStatus::Ok,
            output: input["value"].clone(),
        }
    }
}
struct ServerState {
    requests: Mutex<Vec<Value>>,
    responses: Vec<String>,
}
struct Server {
    state: Arc<ServerState>,
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(responses: Vec<String>) -> Self {
        let state = Arc::new(ServerState {
            requests: Mutex::new(Vec::new()),
            responses,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/v1/responses", post(Self::respond))
            .route("/v1/chat/completions", post(Self::respond))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, base, task }
    }
    async fn respond(
        State(state): State<Arc<ServerState>>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let mut requests = state.requests.lock().unwrap();
        let index = requests.len();
        requests.push(body);
        (
            [("content-type", "text/event-stream")],
            state.responses.get(index).cloned().unwrap_or_else(|| {
                sse(
                    vec![json!({"error":{"message":"unexpected extra request"}})],
                    false,
                )
            }),
        )
    }
    fn runtime(&self, style: ApiStyle, registry: Arc<ToolRegistry>) -> OpenAiCompatRuntime {
        OpenAiCompatRuntime::new(LlmConfig {
            api_base: super::super::super::ApiBase::try_new(&self.base).unwrap(),
            api_key: super::super::super::ApiKey::try_new("test").unwrap(),
            model: super::super::super::ModelId::try_new("test-model").unwrap(),
            api_style: style,
        })
        .with_tools(registry)
    }
}

#[tokio::test]
async fn both_protocols_commit_tools_and_return_results_before_final_response() {
    for style in [ApiStyle::Completions, ApiStyle::Responses] {
        let server = Server::start(vec![
            if style == ApiStyle::Completions {
                chat_calls()
            } else {
                responses_calls(false)
            },
            final_response(style),
        ])
        .await;
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(Compute(executions.clone())))
            .unwrap();
        let registry = Arc::new(registry);
        let runtime = server.runtime(style, registry.clone());
        let prefix = AgentPrefix {
            preamble: Vec::new(),
            tools: registry.specs(),
            skill_index: Default::default(),
        };
        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();
        let user = store
            .record_user(&sid, &MessageContent::text("calculate"))
            .unwrap();
        let job = GenerationJob::new(sid.clone(), user.message_id);
        let directory = SessionDirectory::new(AgentPorts::from_sources(
            Arc::new(runtime),
            Arc::new(FixedAgentPrefix(prefix)),
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::SealAlways)),
        ));
        let (bus, mut events) = tokio::sync::broadcast::channel(64);
        directory.enqueue(&sid, job.clone()).unwrap();
        directory.run_until_idle(&sid, &store, &bus).await.unwrap();
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "unregistered call must never execute"
        );
        let rows = store.load_rows(&sid).unwrap();
        let results: Vec<_> = rows
            .iter()
            .filter_map(|row| {
                if let TurnItem::ToolResult { status, .. } = &row.item {
                    Some(*status)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(results, [ToolResultStatus::Ok, ToolResultStatus::Error]);
        assert!(
            matches!(&rows.last().unwrap().item,TurnItem::Assistant{content} if content=="结果是 2")
        );
        assert_eq!(
            store.session_snapshot(&sid).unwrap().generations[0].status,
            phi_kernel::GenerationStatus::Completed
        );
        let mut usage = false;
        while let Ok(event) = events.try_recv() {
            if matches!(
                event,
                phi_kernel::KernelEvent::GenerationUsage {
                    usage: Usage {
                        total_tokens: Some(16),
                        ..
                    },
                    ..
                }
            ) {
                usage = true;
            }
        }
        assert!(usage, "trailing usage must survive the tool round");
        let requests = server.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        if style == ApiStyle::Completions {
            let messages = requests[1]["messages"].as_array().unwrap();
            let call = messages
                .iter()
                .find(|message| message.get("tool_calls").is_some())
                .unwrap();
            assert_eq!(call["tool_calls"].as_array().unwrap().len(), 2);
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| message["role"] == "tool")
                    .count(),
                2
            );
        } else {
            let items = requests[1]["input"].as_array().unwrap();
            assert!(
                items
                    .iter()
                    .any(|item| item["encrypted_content"] == "opaque-value")
            );
            assert_eq!(
                items
                    .iter()
                    .filter(|item| item["type"] == "function_call_output")
                    .count(),
                2
            );
            let history = store.load_job_history(&job).unwrap();
            assert!(history.iter().any(|item|matches!(item,TurnItem::ModelResponse{response} if response.continuation.is_some())));
        }
    }
}

#[tokio::test]
async fn pull_boundary_does_not_execute_until_consumer_advances() {
    let server = Server::start(vec![chat_calls(), final_response(ApiStyle::Completions)]).await;
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(Compute(executions.clone())))
        .unwrap();
    let registry = Arc::new(registry);
    let runtime = server.runtime(ApiStyle::Completions, registry.clone());
    let mut run = runtime
        .run(TurnRequest {
            session_id: SessionId::generate(),
            job_id: JobId::generate(),
            history: vec![TurnItem::User {
                content: MessageContent::text("calculate"),
            }],
            prefix: AgentPrefix {
                preamble: Vec::new(),
                tools: registry.specs(),
                skill_index: Default::default(),
            },
            cancel: TurnCancel::new(),
            tail_state: None,
            tool_call_seal: ToolCallSealPolicy::SealAlways,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::ModelResponseCompleted { .. }
        ) {
            break;
        }
    }
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(server.state.requests.lock().unwrap().len(), 1);
    run.close_and_join().await.unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn byte_fragmentation_and_duplicate_ids_are_validated_before_execution() {
    for (style, fixture, valid) in [
        (ApiStyle::Completions, chat_calls(), true),
        (ApiStyle::Responses, responses_calls(false), true),
        (ApiStyle::Responses, responses_calls(true), false),
    ] {
        let bytes: Vec<_> = fixture
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(&[*byte])))
            .collect();
        let mut reader = SseReader::from_byte_stream(
            stream::iter(bytes),
            TurnCancel::new(),
            style,
            "scope".into(),
        );
        let mut completed = false;
        let mut error = false;
        while let Some(event) = match reader.next().await {
            Ok(event) => event,
            Err(_) => {
                error = true;
                None
            }
        } {
            if let AgentEvent::ModelResponseCompleted { response } = event {
                completed = true;
                assert!(response.rows.iter().any(
                    |row| matches!(&row.item,TurnItem::Assistant{content} if content=="计算前")
                ));
            }
        }
        assert_eq!(completed, valid);
        assert_eq!(error, !valid);
    }
}

#[test]
fn malformed_arguments_remain_model_correctable_and_incomplete_responses_fail() {
    let mut decoder = ResponseDecoder::new(ApiStyle::Completions, "scope".into());
    let mut events = VecDeque::new();
    decoder.consume(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"broken","function":{"name":"compute","arguments":"{broken"}}]},"finish_reason":"tool_calls"}]}),None,&mut events).unwrap();
    let response = decoder.complete().unwrap();
    assert!(
        matches!(&response.rows[0].item,TurnItem::ToolCall{input,..} if input.parse().is_err()&&input.as_str()=="{broken")
    );
    let mut responses = ResponseDecoder::new(ApiStyle::Responses, "scope".into());
    assert!(
        responses
            .consume(json!({"type":"response.incomplete"}), None, &mut events)
            .is_err()
    );
}

#[tokio::test]
async fn incompatible_continuation_fails_before_network_and_oneshot_never_executes() {
    let server = Server::start(vec![chat_calls()]).await;
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(Compute(executions.clone())))
        .unwrap();
    let registry = Arc::new(registry);
    let runtime = server.runtime(ApiStyle::Responses, registry.clone());
    let response = ModelResponse {
        id: ModelResponseId::generate(),
        rows: Vec::new(),
        complete: true,
        continuation: Some(ProviderContinuation {
            scope: "different-provider".into(),
            payload: json!([]),
        }),
    };
    let mut run = runtime
        .run(TurnRequest {
            session_id: SessionId::generate(),
            job_id: JobId::generate(),
            history: vec![
                TurnItem::User {
                    content: MessageContent::text("continue"),
                },
                TurnItem::ModelResponse { response },
            ],
            prefix: AgentPrefix::baseline_chat(Vec::new()),
            cancel: TurnCancel::new(),
            tail_state: None,
            tool_call_seal: ToolCallSealPolicy::SealAlways,
        })
        .await
        .unwrap();
    assert!(
        run.next()
            .await
            .unwrap()
            .unwrap_err()
            .contains("persisted reasoning")
    );
    run.close_and_join().await.unwrap();
    assert!(server.state.requests.lock().unwrap().is_empty());
    let runtime = server.runtime(ApiStyle::Completions, registry);
    assert!(runtime.complete("name this conversation").await.is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(server.state.requests.lock().unwrap().len(), 1);
}
