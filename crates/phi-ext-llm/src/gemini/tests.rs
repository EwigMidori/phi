//! Synthetic external-protocol fixtures: Gemini GenerateContent v1beta, 2026-09-22.
//! See fixtures/README.md for the exact upstream contract used; no live requests.
use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    ApiBase, ApiKey, AuthMode, LlmRuntime, PreparedImage, PreparedImages, ResponseStep,
    ToolArgumentNormalizer,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, State},
    http::{HeaderMap, Response, StatusCode},
};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use phi_ext_tools::{ToolExecution, ToolExecutor, ToolRegistry};
use phi_kernel::{
    AgentEvent, AgentRun, AgentRuntime, ContentPart, ImageId, JobId, MessageContent, MessageId,
    ModelResponse, OneshotModel, OneshotRequest, OneshotText, ToolArguments, ToolCallId,
    ToolCallSealPolicy, ToolName, ToolResultStatus, ToolSpec, TurnRequest, Usage,
};
use serde_json::json;

struct Reply {
    content_type: &'static str,
    bytes: Vec<u8>,
    stall: bool,
    status: StatusCode,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            content_type: "application/json",
            bytes: serde_json::to_vec(&value).unwrap(),
            stall: false,
            status: StatusCode::OK,
        }
    }
    fn sse(values: &[Value]) -> Self {
        let mut body = String::from(": Gemini fixture\r\n\r\n");
        for value in values {
            body.push_str(&format!("data: {value}\r\n\r\n"));
        }
        Self {
            content_type: "text/event-stream",
            bytes: body.into_bytes(),
            stall: false,
            status: StatusCode::OK,
        }
    }
    fn native(mode: ResponseMode, value: Value) -> Self {
        match mode {
            ResponseMode::Buffered => Self::json(value),
            ResponseMode::Streaming => Self::sse(&[value]),
        }
    }
}
struct Captured {
    uri: String,
    headers: HeaderMap,
    body: Value,
}
struct ServerState {
    requests: Mutex<Vec<Captured>>,
    replies: Mutex<VecDeque<Reply>>,
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
    async fn start(replies: Vec<Reply>) -> Self {
        let state = Arc::new(ServerState {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1beta", listener.local_addr().unwrap());
        let router = Router::new()
            .fallback(Self::respond)
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, base, task }
    }
    async fn respond(
        State(state): State<Arc<ServerState>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response<Body> {
        state.requests.lock().unwrap().push(Captured {
            uri: uri.to_string(),
            headers,
            body,
        });
        let reply = state.replies.lock().unwrap().pop_front().unwrap_or(Reply {
            content_type: "application/json",
            bytes: b"{}".to_vec(),
            stall: false,
            status: StatusCode::INTERNAL_SERVER_ERROR,
        });
        // Split even UTF-8 and CRLF across transport chunks.
        let chunks: Vec<Result<Bytes, std::io::Error>> = reply
            .bytes
            .chunks(3)
            .map(|bytes| Ok(Bytes::copy_from_slice(bytes)))
            .collect();
        let stream: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> =
            if reply.stall {
                Box::pin(futures::stream::iter(chunks).chain(futures::stream::pending()))
            } else {
                Box::pin(futures::stream::iter(chunks))
            };
        Response::builder()
            .status(reply.status)
            .header("content-type", reply.content_type)
            .body(Body::from_stream(stream))
            .unwrap()
    }
    fn protocol(&self, mode: ResponseMode) -> GeminiProtocol {
        GeminiProtocol::new(
            HttpConnection {
                api_base: ApiBase::try_new(&self.base).unwrap(),
                api_key: ApiKey::try_new("fixture-key").unwrap(),
                auth_mode: if mode == ResponseMode::Buffered {
                    AuthMode::Bearer
                } else {
                    AuthMode::GoogleApiKeyHeader
                },
            },
            ModelId::try_new("models/fixture-model").unwrap(),
            mode,
        )
        .unwrap()
        .with_required_thought_signatures(true)
    }
}

fn final_response() -> Value {
    serde_json::from_str(include_str!("fixtures/final.json")).unwrap()
}
fn calls_response() -> Value {
    serde_json::from_str(include_str!("fixtures/calls.json")).unwrap()
}

#[derive(Clone)]
struct Compute {
    executions: Arc<Mutex<Vec<Value>>>,
}
#[async_trait]
impl ToolExecutor for Compute {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("compute"),
            description: "Returns the input number".into(),
            parameters: Some(
                json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}),
            ),
        }
    }
    fn normalize_input(&self, mut input: Value) -> Result<Value, ToolExecution> {
        if let Some(object) = input.as_object_mut()
            && let Some(value) = object.remove("legacyValue")
        {
            object.insert("value".into(), value);
        }
        if input.get("value").and_then(Value::as_i64).is_none() {
            return Err(ToolExecution::error(
                "InvalidArguments",
                "value must be an integer",
            ));
        }
        Ok(input)
    }
    async fn execute(&self, input: Value, _: TurnCancel) -> ToolExecution {
        self.executions.lock().unwrap().push(input.clone());
        ToolExecution {
            status: ToolResultStatus::Ok,
            output: input["value"].clone(),
        }
    }
}
fn registry() -> (Arc<ToolRegistry>, Arc<Mutex<Vec<Value>>>) {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(Compute {
            executions: executions.clone(),
        }))
        .unwrap();
    (Arc::new(registry), executions)
}
fn request(tools: &ToolRegistry) -> TurnRequest {
    TurnRequest {
        session_id: SessionId::generate(),
        job_id: JobId::generate(),
        history: vec![TurnItem::User {
            content: MessageContent::text("Compute two numbers"),
        }],
        tail_state: None,
        prefix: AgentPrefix {
            tools: tools.specs(),
            ..AgentPrefix::default()
        },
        tool_call_seal: ToolCallSealPolicy::SealAlways,
        cancel: TurnCancel::new(),
    }
}
async fn until_response(run: &mut AgentRun, usages: &mut Vec<Usage>) -> ModelResponse {
    loop {
        match run
            .next()
            .await
            .expect("run event")
            .expect("successful event")
        {
            AgentEvent::ModelResponseCompleted { response } => return response,
            AgentEvent::Usage { usage } => usages.push(usage),
            _ => {}
        }
    }
}
fn call_rows(response: &ModelResponse) -> Vec<(ToolCallId, ToolName, ToolArguments)> {
    response
        .rows
        .iter()
        .filter_map(|row| match &row.item {
            TurnItem::ToolCall {
                tool_call_id,
                tool_name,
                input,
            } => Some((tool_call_id.clone(), tool_name.clone(), input.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn both_modes_commit_before_tools_and_replay_native_parts_with_canonical_execution() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let original = calls_response();
        let first = if mode == ResponseMode::Streaming {
            Reply::sse(&[
                json!({"candidates":[{"index":0,"content":{"role":"model","parts":[original["candidates"][0]["content"]["parts"][0].clone()]}}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":1,"totalTokenCount":13}}),
                json!({"candidates":[{"index":0,"content":{"role":"model","parts":original["candidates"][0]["content"]["parts"].as_array().unwrap()[1..].to_vec()},"finishReason":"STOP"}],"usageMetadata":original["usageMetadata"]}),
                json!({"usageMetadata":original["usageMetadata"]}),
            ])
        } else {
            Reply::json(original.clone())
        };
        let server = Server::start(vec![first, Reply::native(mode, final_response())]).await;
        let protocol = server.protocol(mode);
        let (tools, executions) = registry();
        let runtime = LlmRuntime::new(Arc::new(protocol.clone())).with_tools(tools.clone());
        let mut run = runtime.run(request(&tools)).await.unwrap();
        let mut usages = Vec::new();
        let response = until_response(&mut run, &mut usages).await;
        assert!(
            executions.lock().unwrap().is_empty(),
            "response must commit before a tool starts"
        );
        let calls = call_rows(&response);
        assert_eq!(calls.len(), 2);
        assert_ne!(
            calls[0].0, calls[1].0,
            "same-name calls retain separate identities"
        );
        assert_eq!(calls[0].2.parse().unwrap(), json!({"value":3}));
        assert!(matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::ToolResult {
                status: ToolResultStatus::Ok,
                ..
            }
        ));
        assert_eq!(executions.lock().unwrap().len(), 1);
        assert_eq!(
            server.state.requests.lock().unwrap().len(),
            1,
            "first result must commit before more effects"
        );
        assert!(matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::ToolResult {
                status: ToolResultStatus::Ok,
                ..
            }
        ));
        let final_reply = until_response(&mut run, &mut usages).await;
        assert!(final_reply.rows.iter().any(
            |row| matches!(&row.item, TurnItem::Assistant { content } if content == "答案完成")
        ));
        assert!(matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::Finished { .. }
        ));
        run.close_and_join().await.unwrap();
        assert_eq!(
            *executions.lock().unwrap(),
            [json!({"value":3}), json!({"value":7})]
        );
        assert_eq!(
            usages.len(),
            2,
            "one final usage per HTTP response, never per SSE snapshot"
        );
        assert_eq!(
            usages[0],
            Usage::new(Some(11), Some(4), Some(18)).with_cache(Some(2), None)
        );
        let requests = server.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].body["contents"][1], original["candidates"][0]["content"],
            "signed parts and legacy arguments remain immutable"
        );
        let results = requests[1].body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(
            results[0],
            json!({"functionResponse":{"name":"compute","response":{"status":"ok","output":3}}})
        );
        assert_eq!(
            results[1],
            json!({"functionResponse":{"id":"provider-second","name":"compute","response":{"status":"ok","output":7}}})
        );
        match mode {
            ResponseMode::Buffered => {
                assert_eq!(
                    requests[0].uri,
                    "/v1beta/models/fixture-model:generateContent"
                );
                assert_eq!(requests[0].headers["authorization"], "Bearer fixture-key");
                assert!(!requests[0].headers.contains_key("x-goog-api-key"));
            }
            ResponseMode::Streaming => {
                assert_eq!(
                    requests[0].uri,
                    "/v1beta/models/fixture-model:streamGenerateContent?alt=sse"
                );
                assert_eq!(requests[0].headers["x-goog-api-key"], "fixture-key");
                assert!(!requests[0].headers.contains_key("authorization"));
            }
        }
        drop(requests);

        // Persistence compatibility: rehydrate, remap document IDs, and encode a new
        // request. This never restarts the old run or executes unfinished tools.
        let persisted = serde_json::to_vec(&response).unwrap();
        let mut reloaded: ModelResponse = serde_json::from_slice(&persisted).unwrap();
        reloaded.id = phi_kernel::ModelResponseId::generate();
        for row in &mut reloaded.rows {
            row.id = MessageId::generate();
            match &mut row.item {
                TurnItem::ToolCall { tool_call_id, .. } => {
                    *tool_call_id = ToolCallId::new(MessageId::generate().to_string())
                }
                TurnItem::Assistant { content } => *content = "display-only cleaned text".into(),
                _ => {}
            }
        }
        let mut history = vec![
            TurnItem::User {
                content: "Compute two numbers".into(),
            },
            TurnItem::ModelResponse {
                response: reloaded.clone(),
            },
        ];
        for (id, name, args) in call_rows(&reloaded) {
            history.push(TurnItem::ToolResult {
                tool_call_id: id,
                tool_name: name,
                output: args.parse().unwrap()["value"].clone(),
                status: ToolResultStatus::Ok,
            });
        }
        let encoded = protocol
            .request_body(&AgentPrefix::default(), &history, &PreparedImages::new())
            .unwrap();
        assert_eq!(encoded["contents"][1], original["candidates"][0]["content"]);
        if let TurnItem::ModelResponse { response } = &mut history[1] {
            for row in &mut response.rows {
                if let TurnItem::ToolCall { input, .. } = &mut row.item {
                    *input = ToolArguments::from(json!({"value":999}));
                    break;
                }
            }
        }
        assert!(
            protocol
                .request_body(&AgentPrefix::default(), &history, &PreparedImages::new())
                .is_err(),
            "edited arguments cannot silently reuse a signed call"
        );
    }
}

#[tokio::test]
async fn nullable_native_fields_preserve_tool_commit_and_signed_replay() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let mut original = calls_response();
        original["candidates"][0]["index"] = Value::Null;
        original["candidates"][0]["content"]["parts"][0]["thought"] = Value::Null;
        original["candidates"][0]["content"]["parts"][0]["thoughtSignature"] = Value::Null;
        original["candidates"][0]["content"]["parts"][0]["functionCall"] = Value::Null;
        original["candidates"][0]["content"]["parts"][1]["functionCall"]["id"] = Value::Null;
        original["candidates"][0]["content"]["parts"][1]["text"] = Value::Null;
        original["promptFeedback"] = json!({"blockReason":null});
        original["usageMetadata"]["cachedContentTokenCount"] = Value::Null;
        let first = if mode == ResponseMode::Streaming {
            let mut pending = original.clone();
            pending["candidates"][0]["finishReason"] = Value::Null;
            Reply::sse(&[
                json!({"candidates":null,"usageMetadata":null}),
                json!({"candidates":[{"content":null,"finishReason":null}]}),
                json!({"candidates":[{"content":{"role":null,"parts":null}}]}),
                pending,
                json!({"candidates":[{"content":null,"finishReason":"STOP"}]}),
                json!({"usageMetadata":{"promptTokenCount":null,"totalTokenCount":18}}),
            ])
        } else {
            Reply::json(original.clone())
        };
        let server = Server::start(vec![first, Reply::native(mode, final_response())]).await;
        let (tools, executions) = registry();
        let mut run = LlmRuntime::new(Arc::new(server.protocol(mode)))
            .with_tools(tools.clone())
            .run(request(&tools))
            .await
            .unwrap();
        let mut usages = Vec::new();
        let response = until_response(&mut run, &mut usages).await;
        assert!(executions.lock().unwrap().is_empty());
        assert_eq!(call_rows(&response).len(), 2);
        assert!(
            matches!(&response.rows[0].item, TurnItem::Assistant { content } if content == "先计算")
        );
        assert_eq!(usages[0], Usage::new(Some(11), Some(4), Some(18)));
        let mut finished = false;
        while let Some(event) = run.next().await {
            if matches!(event.unwrap(), AgentEvent::Finished { .. }) {
                finished = true;
                break;
            }
        }
        run.close_and_join().await.unwrap();
        assert!(finished);
        assert_eq!(
            *executions.lock().unwrap(),
            [json!({"value":3}), json!({"value":7})]
        );
        let requests = server.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].body["contents"][1], original["candidates"][0]["content"],
            "null tolerance must not rewrite signed parts or tool arguments"
        );
    }
}

#[tokio::test]
async fn unsuccessful_finish_reasons_report_cause_without_sealing_calls() {
    use crate::protocol::ProviderErrorKind;
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        for (reason, kind, diagnostic) in [
            (
                json!("MAX_TOKENS"),
                ProviderErrorKind::Truncated,
                "MAX_TOKENS",
            ),
            (json!("LANGUAGE"), ProviderErrorKind::Blocked, "LANGUAGE"),
            (
                json!("IMAGE_RECITATION"),
                ProviderErrorKind::Blocked,
                "IMAGE_RECITATION",
            ),
            (
                json!("MODEL_ARMOR"),
                ProviderErrorKind::Blocked,
                "MODEL_ARMOR",
            ),
            (json!("OTHER"), ProviderErrorKind::Protocol, "OTHER"),
            (
                json!("MALFORMED_RESPONSE"),
                ProviderErrorKind::Protocol,
                "MALFORMED_RESPONSE",
            ),
            (
                json!("FUTURE_REASON"),
                ProviderErrorKind::Protocol,
                "FUTURE_REASON",
            ),
            (json!(false), ProviderErrorKind::Protocol, "boolean"),
            (
                json!({"secret":"private"}),
                ProviderErrorKind::Protocol,
                "object",
            ),
            (
                json!("private\ntext"),
                ProviderErrorKind::Protocol,
                "invalid enum name",
            ),
            (
                Value::Null,
                ProviderErrorKind::Protocol,
                "without a complete stopped candidate",
            ),
        ] {
            let mut body = calls_response();
            body["candidates"][0]["finishReason"] = reason;
            let server = Server::start(vec![Reply::native(mode, body)]).await;
            let (tools, _) = registry();
            let request = request(&tools);
            let mut response = server
                .protocol(mode)
                .open_response(
                    &request.session_id,
                    &request.prefix,
                    &request.history,
                    &PreparedImages::default(),
                    &request.cancel,
                )
                .await
                .unwrap();
            let error = loop {
                match response.next().await {
                    Ok(ResponseStep::Observation(_)) => {}
                    Err(error) => break error,
                    _ => panic!("unsuccessful response became executable"),
                }
            };
            assert_eq!(error.kind(), kind);
            assert!(error.to_string().contains(diagnostic), "{error}");
            assert!(!error.to_string().contains("private"));
            assert!(matches!(
                response.next().await.unwrap(),
                ResponseStep::Ended
            ));
        }
    }
}

#[tokio::test]
async fn malformed_truncated_blocked_or_unsigned_responses_never_execute_calls() {
    let mut duplicate = calls_response();
    duplicate["candidates"][0]["content"]["parts"][1]["functionCall"]["id"] =
        json!("provider-second");
    let mut truncated = calls_response();
    truncated["candidates"][0]["finishReason"] = json!("MAX_TOKENS");
    let mut unsigned = calls_response();
    unsigned["candidates"][0]["content"]["parts"][1]
        .as_object_mut()
        .unwrap()
        .remove("thoughtSignature");
    let mut null_signature = unsigned.clone();
    null_signature["candidates"][0]["content"]["parts"][1]["thoughtSignature"] = Value::Null;
    let mut invalid_thought = calls_response();
    invalid_thought["candidates"][0]["content"]["parts"][0]["thought"] = json!("false");
    let mut unfinished = calls_response();
    unfinished["candidates"][0]
        .as_object_mut()
        .unwrap()
        .remove("finishReason");
    let blocked = json!({"promptFeedback":{"blockReason":"SAFETY"},"usageMetadata":{"promptTokenCount":6,"totalTokenCount":6}});
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        for response in [
            duplicate.clone(),
            truncated.clone(),
            unsigned.clone(),
            null_signature.clone(),
            invalid_thought.clone(),
            unfinished.clone(),
            blocked.clone(),
            json!({"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"STOP"}]}),
        ] {
            let server = Server::start(vec![Reply::native(mode, response)]).await;
            let (tools, executions) = registry();
            let runtime =
                LlmRuntime::new(Arc::new(server.protocol(mode))).with_tools(tools.clone());
            let mut run = runtime.run(request(&tools)).await.unwrap();
            let mut failed = false;
            while let Some(event) = run.next().await {
                match event {
                    Err(_) => {
                        failed = true;
                        break;
                    }
                    Ok(
                        AgentEvent::ModelResponseCompleted { .. }
                        | AgentEvent::ToolResult { .. }
                        | AgentEvent::Finished { .. },
                    ) => panic!("invalid response claimed completion"),
                    _ => {}
                }
            }
            run.close_and_join().await.unwrap();
            assert!(failed);
            assert!(executions.lock().unwrap().is_empty());
            assert_eq!(server.state.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn usage_only_drain_discards_even_invalid_tool_payloads_without_a_second_request() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let reply = if mode == ResponseMode::Buffered {
            Reply::json(calls_response())
        } else {
            Reply::sse(&[
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"accepted answer"}]}}]}),
                json!({"candidates":[{"content":{"parts":[{"functionCall":{"partialArgs":"invalid discarded extension"}}]}}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":4,"totalTokenCount":18}}),
                json!({"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":4,"totalTokenCount":18}}),
            ])
        };
        let server = Server::start(vec![reply]).await;
        let (tools, executions) = registry();
        let mut run = LlmRuntime::new(Arc::new(server.protocol(mode)))
            .with_tools(tools.clone())
            .run(request(&tools))
            .await
            .unwrap();
        while !matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::TextDelta { .. }
        ) {}
        run.usage_drain().unwrap().begin();
        let mut usages = 0;
        while let Some(event) = run.next().await {
            match event.unwrap() {
                AgentEvent::Usage { usage } => {
                    usages += 1;
                    assert_eq!(usage.total_tokens, Some(18));
                }
                AgentEvent::Finished { .. } => break,
                _ => panic!("output escaped usage-only mode"),
            }
        }
        run.close_and_join().await.unwrap();
        assert_eq!(usages, 1);
        assert!(executions.lock().unwrap().is_empty());
        assert_eq!(server.state.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn both_modes_oneshot_use_native_reader_and_cancel_pending_body() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let server = Server::start(vec![
            Reply::native(mode, final_response()),
            Reply::native(mode, calls_response()),
        ])
        .await;
        let runtime = LlmRuntime::new(Arc::new(server.protocol(mode)));
        assert_eq!(runtime.complete("answer").await.unwrap(), "答案完成");
        assert!(runtime.complete("unexpected tools").await.is_err());

        let mut pending = Reply::native(mode, json!({}));
        pending.bytes.clear();
        pending.stall = true;
        let server = Server::start(vec![pending]).await;
        let runtime = LlmRuntime::new(Arc::new(server.protocol(mode)));
        let cancel = TurnCancel::new();
        let mut run = runtime
            .generate(OneshotRequest {
                session_id: SessionId::generate(),
                input: "cancel".into(),
                instructions: Vec::new(),
                cancel: cancel.clone(),
            })
            .await
            .unwrap();
        assert!(matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::ResponseStarted { .. }
        ));
        let cancellation = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel.cancel();
        });
        let event = tokio::time::timeout(Duration::from_secs(2), run.next())
            .await
            .unwrap()
            .unwrap();
        assert!(event.is_err());
        cancellation.await.unwrap();
        run.close_and_join().await.unwrap();
        assert_eq!(server.state.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn stream_late_empty_signature_and_repeated_text_are_preserved_without_done_sentinel() {
    let values = [
        json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"哈"}]}}]}),
        json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"哈"}]},"finishReason":"STOP"}]}),
        json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"","thoughtSignature":"c2lnbmF0dXJl"}]}}],"usageMetadata":{"totalTokenCount":9}}),
    ];
    let server = Server::start(vec![Reply::sse(&values)]).await;
    let mut run = LlmRuntime::new(Arc::new(server.protocol(ResponseMode::Streaming)))
        .generate(OneshotRequest {
            session_id: SessionId::generate(),
            input: "repeat".into(),
            instructions: Vec::new(),
            cancel: TurnCancel::new(),
        })
        .await
        .unwrap();
    let mut usages = Vec::new();
    let response = until_response(&mut run, &mut usages).await;
    assert!(matches!(&response.rows[0].item, TurnItem::Assistant { content } if content == "哈哈"));
    assert_eq!(
        response.continuation.unwrap().payload["content"]["parts"],
        json!([{"text":"哈"},{"text":"哈"},{"text":"","thoughtSignature":"c2lnbmF0dXJl"}])
    );
    assert_eq!(usages, [Usage::new(None, None, Some(9))]);
    run.close_and_join().await.unwrap();
}

struct IdentityNormalizer;
impl ToolArgumentNormalizer for IdentityNormalizer {
    fn normalize(&self, _: &ToolName, original: &ToolArguments) -> Result<ToolArguments, String> {
        Ok(original.clone())
    }
}

#[tokio::test]
async fn signed_history_preflight_distinguishes_current_turn_from_old_foreign_history() {
    let server = Server::start(vec![]).await;
    let protocol = server.protocol(ResponseMode::Buffered);
    let id = ToolCallId::new("foreign-call");
    let mut history = vec![
        TurnItem::User {
            content: "old question".into(),
        },
        TurnItem::ToolCall {
            tool_call_id: id.clone(),
            tool_name: ToolName::new("compute"),
            input: ToolArguments::from(json!({"value":3})),
        },
        TurnItem::ToolResult {
            tool_call_id: id,
            tool_name: ToolName::new("compute"),
            output: json!(3),
            status: ToolResultStatus::Ok,
        },
    ];
    assert!(
        protocol
            .request_body(&AgentPrefix::default(), &history, &PreparedImages::new())
            .is_err()
    );
    history.push(TurnItem::User {
        content: "new question".into(),
    });
    assert!(
        protocol
            .request_body(&AgentPrefix::default(), &history, &PreparedImages::new())
            .is_ok()
    );
    assert!(server.state.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn inline_images_and_thinking_controls_use_native_semantics() {
    let server = Server::start(vec![]).await;
    let image = ImageId::generate();
    let mut prepared = PreparedImages::new();
    prepared.insert(
        image.clone(),
        PreparedImage::Inline {
            mime_type: "image/png".into(),
            bytes: Bytes::from_static(b"fixture bytes"),
        },
    );
    let history = [TurnItem::User {
        content: MessageContent::from_parts(vec![
            ContentPart::Text {
                text: "inspect".into(),
            },
            ContentPart::Image { image_id: image },
        ]),
    }];
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        for thinking in [
            GeminiThinking::Budget(0),
            GeminiThinking::Budget(-1),
            GeminiThinking::Budget(1024),
            GeminiThinking::Level(ReasoningEffort::Low),
        ] {
            let protocol = server.protocol(mode).with_thinking(thinking).unwrap();
            let body = protocol
                .request_body(&AgentPrefix::default(), &history, &prepared)
                .unwrap();
            assert_eq!(
                body["contents"][0]["parts"][1],
                json!({"inlineData":{"mimeType":"image/png","data":"Zml4dHVyZSBieXRlcw=="}})
            );
            let config = &body["generationConfig"]["thinkingConfig"];
            match thinking {
                GeminiThinking::Budget(budget) => {
                    assert_eq!(config["thinkingBudget"], json!(budget));
                    assert!(config.get("thinkingLevel").is_none());
                }
                GeminiThinking::Level(_) => {
                    assert_eq!(config["thinkingLevel"], "low");
                    assert!(config.get("thinkingBudget").is_none());
                }
                _ => unreachable!(),
            }
        }
        assert!(
            server
                .protocol(mode)
                .with_thinking(GeminiThinking::Level(ReasoningEffort::Max))
                .is_err()
        );
        assert!(
            server
                .protocol(mode)
                .with_thinking(GeminiThinking::Budget(-2))
                .is_err()
        );
    }
}

#[tokio::test]
async fn invalid_tool_inputs_and_unknown_names_return_results_the_model_can_correct() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let mut first = calls_response();
        first["candidates"][0]["content"]["parts"][1]["functionCall"]["args"] =
            json!({"legacyValue":"not an integer"});
        first["candidates"][0]["content"]["parts"][2]["functionCall"]["name"] =
            json!("not_enabled");
        let server = Server::start(vec![
            Reply::native(mode, first),
            Reply::native(mode, final_response()),
        ])
        .await;
        let (tools, executions) = registry();
        let mut run = LlmRuntime::new(Arc::new(server.protocol(mode)))
            .with_tools(tools.clone())
            .run(request(&tools))
            .await
            .unwrap();
        let mut results = Vec::new();
        while let Some(event) = run.next().await {
            match event.unwrap() {
                AgentEvent::ToolResult { status, .. } => results.push(status),
                AgentEvent::Finished { .. } => break,
                _ => {}
            }
        }
        run.close_and_join().await.unwrap();
        assert_eq!(results, [ToolResultStatus::Error, ToolResultStatus::Error]);
        assert!(executions.lock().unwrap().is_empty());
        assert_eq!(server.state.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn multiple_tool_rounds_keep_each_native_group_and_only_finish_after_the_answer() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let server = Server::start(vec![
            Reply::native(mode, calls_response()),
            Reply::native(mode, calls_response()),
            Reply::native(mode, final_response()),
        ])
        .await;
        let (tools, executions) = registry();
        let mut run = LlmRuntime::new(Arc::new(server.protocol(mode)))
            .with_tools(tools.clone())
            .run(request(&tools))
            .await
            .unwrap();
        let mut responses = 0;
        let mut finished = false;
        while let Some(event) = run.next().await {
            match event.unwrap() {
                AgentEvent::ModelResponseCompleted { .. } => responses += 1,
                AgentEvent::Finished { .. } => {
                    finished = true;
                    assert_eq!(responses, 3);
                    break;
                }
                _ => {}
            }
        }
        run.close_and_join().await.unwrap();
        assert!(finished);
        assert_eq!(executions.lock().unwrap().len(), 4);
        let requests = server.state.requests.lock().unwrap();
        let contents = requests[2].body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 5);
        assert_eq!(contents[1], calls_response()["candidates"][0]["content"]);
        assert_eq!(contents[3], contents[1]);
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[4]["parts"].as_array().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn complete_json_and_sse_with_unfinished_frame_fail_without_a_model_commit() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let mut reply = Reply::native(mode, calls_response());
        if mode == ResponseMode::Buffered {
            reply.bytes.pop();
        } else {
            reply.bytes.extend_from_slice(b"data: {\"unfinished\":");
        }
        let server = Server::start(vec![reply]).await;
        let (tools, executions) = registry();
        let mut run = LlmRuntime::new(Arc::new(server.protocol(mode)))
            .with_tools(tools.clone())
            .run(request(&tools))
            .await
            .unwrap();
        let mut failed = false;
        while let Some(event) = run.next().await {
            match event {
                Err(_) => {
                    failed = true;
                    break;
                }
                Ok(AgentEvent::ModelResponseCompleted { .. }) => {
                    panic!("incomplete transport committed calls")
                }
                _ => {}
            }
        }
        run.close_and_join().await.unwrap();
        assert!(failed);
        assert!(executions.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn response_owner_seals_once_and_cannot_be_revived_after_close() {
    let server = Server::start(vec![Reply::json(final_response())]).await;
    let protocol = server.protocol(ResponseMode::Buffered);
    let mut response = protocol
        .open_response(
            &SessionId::generate(),
            &AgentPrefix::default(),
            &[TurnItem::User {
                content: "answer".into(),
            }],
            &PreparedImages::new(),
            &TurnCancel::new(),
        )
        .await
        .unwrap();
    assert!(response.seal(&IdentityNormalizer).is_err());
    loop {
        if matches!(response.next().await.unwrap(), ResponseStep::ReadyToSeal) {
            break;
        }
    }
    assert!(response.seal(&IdentityNormalizer).unwrap().complete);
    assert!(response.seal(&IdentityNormalizer).is_err());
    response.close();
    assert!(matches!(
        response.next().await.unwrap(),
        ResponseStep::Ended
    ));
    assert!(response.seal(&IdentityNormalizer).is_err());
}

struct InlineSource {
    reads: AtomicUsize,
}
#[async_trait]
impl crate::ImageSource for InlineSource {
    async fn metadata(&self, _: &SessionId, _: &ImageId) -> Result<crate::ImageMetadata, String> {
        Ok(crate::ImageMetadata {
            digest: crate::ImageDigest::of(b"fixture bytes"),
            mime_type: "image/png".into(),
            byte_len: 13,
            width: 1,
            height: 1,
        })
    }
    async fn read(&self, _: &SessionId, _: &ImageId) -> Result<Bytes, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(Bytes::from_static(b"fixture bytes"))
    }
}
struct NoFiles;
impl crate::FileReferenceCache for NoFiles {
    fn lookup(&self, _: &crate::FileCacheKey) -> Result<Option<crate::FileReference>, String> {
        panic!("native inline must not query a foreign Files cache")
    }
    fn remember(&self, _: &crate::FileCacheKey, _: &crate::FileReference) -> Result<(), String> {
        panic!("native inline must not upload files")
    }
    fn forget(&self, _: &crate::FileCacheKey, _: &crate::ProviderFileId) -> Result<(), String> {
        panic!("native inline must not repair foreign files")
    }
}
fn image_policy(max_request_bytes: usize) -> crate::ImagePolicy {
    crate::ImagePolicy {
        enabled: true,
        transfer: Some(crate::ImageTransfer::Inline),
        max_images: 10,
        max_image_bytes: 1024,
        max_total_bytes: 4096,
        max_dimension: 100,
        many_images_dimension: None,
        max_request_bytes,
        file_lifetime: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn actual_native_body_limit_includes_text_and_inline_bytes_without_repeating_preparation() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let server = Server::start(vec![]).await;
        let source = Arc::new(InlineSource {
            reads: AtomicUsize::new(0),
        });
        let images =
            Arc::new(crate::ProviderImages::new(source.clone(), Arc::new(NoFiles)).unwrap());
        let runtime =
            LlmRuntime::new(Arc::new(server.protocol(mode))).with_images(images, image_policy(256));
        let input = MessageContent::from_parts(vec![
            ContentPart::Text {
                text: "large text ".repeat(80),
            },
            ContentPart::Image {
                image_id: ImageId::generate(),
            },
        ]);
        let session_id = SessionId::generate();
        runtime
            .validate_images(
                &session_id,
                &[TurnItem::User {
                    content: input.clone(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            0,
            "metadata preflight must not read/upload the asset"
        );
        assert!(
            runtime
                .generate(OneshotRequest {
                    session_id,
                    input,
                    instructions: Vec::new(),
                    cancel: TurnCancel::new()
                })
                .await
                .is_err()
        );
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            1,
            "protocol validation and sending share prepared bytes"
        );
        assert!(server.state.requests.lock().unwrap().is_empty());
    }
}

struct ToolPlots(ImageId);
impl crate::ToolOutputImages for ToolPlots {
    fn images(&self, _: &ToolName, _: &Value) -> Result<Vec<ImageId>, String> {
        Ok(vec![self.0.clone()])
    }
}

#[tokio::test]
async fn tool_images_stay_inside_function_responses_across_signed_tool_rounds() {
    for mode in [ResponseMode::Buffered, ResponseMode::Streaming] {
        let server = Server::start(vec![
            Reply::native(mode, calls_response()),
            Reply::native(mode, calls_response()),
            Reply::native(mode, final_response()),
        ])
        .await;
        let (tools, executions) = registry();
        let source = Arc::new(InlineSource {
            reads: AtomicUsize::new(0),
        });
        let images =
            Arc::new(crate::ProviderImages::new(source.clone(), Arc::new(NoFiles)).unwrap());
        let runtime = LlmRuntime::new(Arc::new(server.protocol(mode)))
            .with_tools(tools.clone())
            .with_images(images, image_policy(100_000))
            .with_tool_images(Arc::new(ToolPlots(ImageId::generate())));
        let mut run = runtime.run(request(&tools)).await.unwrap();
        let mut finished = false;
        while let Some(event) = run.next().await {
            if matches!(event.unwrap(), AgentEvent::Finished { .. }) {
                finished = true;
                break;
            }
        }
        run.close_and_join().await.unwrap();
        assert!(finished);
        assert_eq!(executions.lock().unwrap().len(), 4);
        let requests = server.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let contents = requests[2].body["contents"].as_array().unwrap();
        assert_eq!(
            contents.len(),
            5,
            "tool-produced images must not create a new user turn"
        );
        for index in [1, 3] {
            assert_eq!(
                contents[index],
                calls_response()["candidates"][0]["content"]
            );
        }
        for index in [2, 4] {
            for part in contents[index]["parts"].as_array().unwrap() {
                let response = &part["functionResponse"];
                assert!(response.is_object());
                assert_eq!(
                    response["parts"],
                    json!([{"inlineData":{"mimeType":"image/png","data":"Zml4dHVyZSBieXRlcw=="}}])
                );
                assert!(
                    response["response"]["imageContext"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty())
                );
            }
        }
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            2,
            "each request prepares shared image bytes once"
        );
    }
}
