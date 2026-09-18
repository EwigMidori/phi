//! OpenAI-compatible protocol fixtures, verified against function-calling docs
//! 2026-09-11. Assertions exercise continuation and execution, not serde derives.
use super::*;
use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use phi_ext_tools::{ToolExecutor, ToolRegistry};
use phi_kernel::{
    AgentPorts, FixedAgentPrefix, FixedToolCallSeal, GenerationJob, InMemoryTranscript, JobId,
    SessionDirectory, ToolCallSealPolicy, ToolResultStatus, Transcript,
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

struct DialogueOnlyProjection;
impl HistoryProjector for DialogueOnlyProjection {
    fn project(&self, _: &[TurnItem]) -> Vec<TurnItem> {
        panic!("standalone completions must not enter dialogue history projection")
    }
}

#[tokio::test]
async fn standalone_completion_bypasses_dialogue_and_collects_trailing_usage() {
    for style in [ApiStyle::Completions, ApiStyle::Responses] {
        let response = match style {
            ApiStyle::Completions => sse(
                vec![
                    json!({"choices":[{"delta":{"content":"完成"}}]}),
                    json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
                    json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}}),
                ],
                true,
            ),
            ApiStyle::Responses => sse(
                vec![json!({"type":"response.completed","response":{
                    "output":[{"type":"message","id":"answer","role":"assistant","content":[{"type":"output_text","text":"完成"}]}],
                    "usage":{"input_tokens":12,"output_tokens":4,"total_tokens":16}
                }})],
                false,
            ),
        };
        let server = Server::start(vec![response, final_response(style)]).await;
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(Compute(executions.clone())))
            .unwrap();
        let runtime = server
            .runtime(style, Arc::new(registry))
            .with_projector(Arc::new(DialogueOnlyProjection));
        let mut run = runtime
            .generate(OneshotRequest {
                session_id: SessionId::generate(),
                input: "材料".into(),
                instructions: vec![phi_kernel::PreambleSection::new("task", "只总结材料")],
                cancel: TurnCancel::new(),
            })
            .await
            .unwrap();
        let mut text = String::new();
        let mut usage = Vec::new();
        let mut finished = false;
        while let Some(event) = run.next().await {
            match event.unwrap() {
                AgentEvent::ModelResponseCompleted { response } => {
                    for row in response.rows {
                        if let TurnItem::Assistant { content } = row.item {
                            text.push_str(&content);
                        }
                    }
                }
                AgentEvent::Usage { usage: value } => usage.push(value),
                AgentEvent::Finished { .. } => finished = true,
                _ => {}
            }
        }
        run.close_and_join().await.unwrap();
        assert_eq!(text, "完成");
        assert!(finished);
        assert_eq!(usage[0].total_tokens, Some(16));
        assert_eq!(runtime.complete("给材料命名").await.unwrap(), "结果是 2");
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        let requests = server.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|body| body.get("tools").is_none()));
        match style {
            ApiStyle::Completions => {
                assert_eq!(
                    requests[0]["messages"][0]["content"],
                    "<task>只总结材料</task>"
                );
                assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 1);
                assert_eq!(requests[1]["messages"][0]["role"], "user");
            }
            ApiStyle::Responses => {
                assert_eq!(requests[0]["instructions"], "<task>只总结材料</task>");
                assert!(requests[1].get("instructions").is_none());
            }
        }
    }
}

#[tokio::test]
async fn standalone_completion_rejects_tool_requests_without_execution_or_continuation() {
    for style in [ApiStyle::Completions, ApiStyle::Responses] {
        let server = Server::start(vec![if style == ApiStyle::Completions {
            chat_calls()
        } else {
            responses_calls(false)
        }])
        .await;
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(Compute(executions.clone())))
            .unwrap();
        let runtime = server.runtime(style, Arc::new(registry));
        let failure = runtime.complete("不执行工具").await.unwrap_err();
        assert!(failure.contains("no tools enabled"), "{failure}");
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert_eq!(server.state.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn standalone_completion_never_accepts_partial_text_as_success() {
    let server = Server::start(vec![sse(
        vec![json!({"choices":[{"delta":{"content":"unfinished"}}]})],
        false,
    )])
    .await;
    let runtime = server.runtime(ApiStyle::Completions, Arc::new(ToolRegistry::new()));
    assert!(runtime.complete("summarize").await.is_err());
    assert_eq!(server.state.requests.lock().unwrap().len(), 1);
}
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

#[tokio::test]
async fn usage_only_drain_reads_both_protocol_tails_without_tools_or_another_request() {
    for style in [ApiStyle::Completions, ApiStyle::Responses] {
        for after_batch in [false, true] {
            let server = Server::start(vec![if style == ApiStyle::Completions {
                chat_calls()
            } else {
                responses_calls(false)
            }])
            .await;
            let executions = Arc::new(AtomicUsize::new(0));
            let mut registry = ToolRegistry::new();
            registry
                .register(Arc::new(Compute(executions.clone())))
                .unwrap();
            let registry = Arc::new(registry);
            let mut run = server
                .runtime(style, registry.clone())
                .run(TurnRequest {
                    session_id: SessionId::generate(),
                    job_id: JobId::generate(),
                    history: vec![TurnItem::User {
                        content: "calculate".into(),
                    }],
                    prefix: AgentPrefix {
                        tools: registry.specs(),
                        ..Default::default()
                    },
                    cancel: TurnCancel::new(),
                    tail_state: None,
                    tool_call_seal: ToolCallSealPolicy::SealAlways,
                })
                .await
                .unwrap();
            loop {
                let event = run.next().await.unwrap().unwrap();
                if (after_batch && matches!(event, AgentEvent::ModelResponseCompleted { .. }))
                    || (!after_batch && matches!(event, AgentEvent::TextDelta { .. }))
                {
                    break;
                }
            }
            // After-batch mode also covers already queued, executable calls.
            run.usage_drain().unwrap().begin();
            let mut usage = None;
            let mut finished = false;
            while let Some(event) = run.next().await {
                match event.unwrap() {
                    AgentEvent::Usage { usage: reported } => usage = Some(reported),
                    AgentEvent::Finished { .. } => finished = true,
                    other => panic!("usage drain leaked an event: {other:?}"),
                }
            }
            assert!(finished);
            if !after_batch {
                assert_eq!(usage.unwrap(), Usage::new(Some(12), Some(4), Some(16)));
            }
            run.close_and_join().await.unwrap();
            assert_eq!(executions.load(Ordering::SeqCst), 0);
            assert_eq!(server.state.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn usage_drain_ignores_output_validation_and_bounds_the_unwanted_tail() {
    let mut reader = SseReader::from_byte_stream(
        stream::iter(
            responses_calls(true)
                .into_bytes()
                .into_iter()
                .map(|byte| Ok(Bytes::from(vec![byte]))),
        ),
        TurnCancel::new(),
        ApiStyle::Responses,
        "test".into(),
    );
    while !matches!(
        reader.next().await.unwrap(),
        Some(AgentEvent::TextDelta { .. })
    ) {}
    let accepted = reader.decoder.text.clone();
    reader.begin_usage_drain();
    assert!(
        matches!(reader.next().await.unwrap(), Some(AgentEvent::Usage { usage }) if usage.total_tokens == Some(16))
    );
    assert!(reader.next().await.unwrap().is_none());
    assert_eq!(reader.decoder.text, accepted);

    let tail = sse(
        vec![
            json!({"choices":[{"delta":{"content":"x".repeat(256 * 1024)}}]}),
            json!({"usage":{"total_tokens":16}}),
        ],
        true,
    );
    let mut reader = SseReader::from_byte_stream(
        stream::iter([Ok(Bytes::from(tail))]),
        TurnCancel::new(),
        ApiStyle::Completions,
        "test".into(),
    );
    reader.begin_usage_drain();
    assert!(reader.next().await.unwrap().is_none());
    assert!(reader.decoder.text.is_empty());

    // Capture a report within the budget even when a large trailing transport
    // chunk also contains unwanted output beyond that budget.
    let tail = sse(
        vec![
            json!({"usage":{"total_tokens":16}}),
            json!({"choices":[{"delta":{"content":"x".repeat(256 * 1024)}}]}),
        ],
        true,
    );
    let mut reader = SseReader::from_byte_stream(
        stream::iter([Ok(Bytes::from(tail))]),
        TurnCancel::new(),
        ApiStyle::Completions,
        "test".into(),
    );
    reader.begin_usage_drain();
    assert!(
        matches!(reader.next().await.unwrap(), Some(AgentEvent::Usage { usage }) if usage.total_tokens == Some(16))
    );
    assert!(reader.next().await.unwrap().is_none());
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

#[test]
fn continuation_replay_projects_visible_text_without_changing_opaque_items() {
    // Responses protocol fixture: reasoning and tool correlation must survive
    // a host projection across multiple message and output_text items.
    let original = json!([
        {"type":"reasoning","id":"reasoning","summary":[],"encrypted_content":"opaque"},
        {"type":"message","id":"first","role":"assistant","content":[
            {"type":"output_text","text":"hello ","annotations":[]},
            {"type":"output_text","text":"world","annotations":[]}
        ]},
        {"type":"function_call","id":"call-item","call_id":"call","name":"compute","arguments":"{\"value\":2}"},
        {"type":"message","id":"second","role":"assistant","content":[
            {"type":"output_text","text":"after tool","annotations":[]}
        ]}
    ]);
    let mut decoder = ResponseDecoder::new(ApiStyle::Responses, "scope".into());
    decoder
        .consume(
            json!({"type":"response.completed","response":{"output":original}}),
            None,
            &mut VecDeque::new(),
        )
        .unwrap();
    let mut response = decoder.complete().unwrap();
    let stored = response.clone();
    let codec = WireCodec::for_style(ApiStyle::Responses);
    let mut unchanged = Vec::new();
    codec
        .encode_history(
            &TurnItem::ModelResponse {
                response: response.clone(),
            },
            &PreparedImages::new(),
            "scope",
            &mut unchanged,
        )
        .unwrap();
    assert_eq!(unchanged, original.as_array().unwrap().clone());

    for row in &mut response.rows {
        if let TurnItem::Assistant { content } = &mut row.item {
            *content = format!("[Alice] {content}");
        }
    }
    let mut replay = Vec::new();
    codec
        .encode_history(
            &TurnItem::ModelResponse {
                response: response.clone(),
            },
            &PreparedImages::new(),
            "scope",
            &mut replay,
        )
        .unwrap();
    let mut expected = original.clone();
    expected[1]["content"][0]["text"] = json!("[Alice] hello world");
    expected[1]["content"][1]["text"] = json!("");
    expected[3]["content"][0]["text"] = json!("[Alice] after tool");
    assert_eq!(replay, expected.as_array().unwrap().clone());
    assert_eq!(response.continuation, stored.continuation);

    response
        .rows
        .retain(|row| !matches!(row.item, TurnItem::Assistant { .. }));
    assert!(
        codec
            .encode_history(
                &TurnItem::ModelResponse { response },
                &PreparedImages::new(),
                "scope",
                &mut Vec::new(),
            )
            .unwrap_err()
            .contains("do not match")
    );

    let mut extra = stored;
    extra.rows.push(TranscriptRow::new(
        MessageId::generate(),
        TurnItem::Assistant {
            content: "unmatched".into(),
        },
    ));
    assert!(
        codec
            .encode_history(
                &TurnItem::ModelResponse { response: extra },
                &PreparedImages::new(),
                "scope",
                &mut Vec::new(),
            )
            .unwrap_err()
            .contains("do not match")
    );
}

#[tokio::test]
async fn foreign_continuation_projects_visible_rows_and_still_requests() {
    for style in [ApiStyle::Completions, ApiStyle::Responses] {
        let server = Server::start(vec![final_response(style)]).await;
        let runtime = server.runtime(style, Arc::new(ToolRegistry::new()));
        let response = ModelResponse {
            id: ModelResponseId::generate(),
            rows: vec![TranscriptRow::new(
                MessageId::generate(),
                TurnItem::Assistant {
                    content: "visible from other model".into(),
                },
            )],
            complete: true,
            continuation: Some(ProviderContinuation {
                scope: "different-provider".into(),
                payload: json!([{"type":"reasoning","encrypted_content":"must-not-leak"}]),
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
        while let Some(item) = run.next().await {
            item.unwrap();
        }
        run.close_and_join().await.unwrap();
        let requests = server.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let encoded = if style == ApiStyle::Responses {
            serde_json::to_string(&requests[0]["input"]).unwrap()
        } else {
            serde_json::to_string(&requests[0]["messages"]).unwrap()
        };
        assert!(encoded.contains("visible from other model"));
        assert!(!encoded.contains("must-not-leak"));
    }
}
