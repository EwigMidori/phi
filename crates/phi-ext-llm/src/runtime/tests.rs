//! A third, synthetic protocol exercises the public extension boundary, not a production enum.
use super::*;
use crate::{ApiBase, ApiKey, AuthMode, HttpConnection, ProviderErrorKind, ResponseStep};
use phi_ext_tools::{ToolExecution, ToolExecutor, ToolRegistry};
use phi_kernel::{
    FailureDisposition, JobId, MessageId, ModelResponseId, ToolArguments, ToolCallId,
    ToolCallSealPolicy, ToolName, ToolResultStatus, ToolSpec, TranscriptRow, TurnRequest, Usage,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Facts {
    opens: AtomicUsize,
    seals: AtomicUsize,
    closes: AtomicUsize,
    executions: Mutex<Vec<Value>>,
    canonical: Mutex<Vec<(ToolArguments, ToolArguments)>>,
}
struct Arithmetic(Arc<Facts>);
#[async_trait]
impl ToolExecutor for Arithmetic {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("arithmetic"),
            description: "normalize legacy input".into(),
            parameters: None,
        }
    }
    fn normalize_input(&self, value: Value) -> Result<Value, ToolExecution> {
        let number = value
            .get("number")
            .or_else(|| value.get("legacy"))
            .and_then(Value::as_i64)
            .ok_or_else(|| ToolExecution::error("Input", "number required"))?;
        Ok(json!({"number": number}))
    }
    async fn execute(&self, input: Value, _: TurnCancel) -> ToolExecution {
        self.0.executions.lock().unwrap().push(input.clone());
        ToolExecution {
            status: ToolResultStatus::Ok,
            output: input,
        }
    }
}
struct SyntheticProtocol {
    connection: HttpConnection,
    facts: Arc<Facts>,
    failure: Option<ProviderErrorKind>,
}
impl SyntheticProtocol {
    fn new(facts: Arc<Facts>) -> Self {
        Self {
            connection: HttpConnection {
                api_base: ApiBase::try_new("https://fixture.invalid").unwrap(),
                api_key: ApiKey::try_new("fixture").unwrap(),
                auth_mode: AuthMode::Bearer,
            },
            facts,
            failure: None,
        }
    }
}
#[async_trait]
impl ProviderProtocol for SyntheticProtocol {
    fn connection(&self) -> &HttpConnection {
        &self.connection
    }
    fn validate_request(
        &self,
        _: &SessionId,
        _: &AgentPrefix,
        _: &[TurnItem],
        _: &PreparedImages,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn open_response(
        &self,
        _: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        _: &PreparedImages,
        cancel: &TurnCancel,
    ) -> Result<Box<dyn ProviderResponse>, ProviderError> {
        self.facts.opens.fetch_add(1, Ordering::SeqCst);
        if let Some(kind) = self.failure {
            return Err(match kind {
                ProviderErrorKind::Transport => ProviderError::transport("fixture network failure"),
                ProviderErrorKind::Http(status) => ProviderError::http(status),
                _ => ProviderError::continuation("fixture history conflict"),
            });
        }
        let calls = !prefix.tools.is_empty()
            && !history
                .iter()
                .any(|item| matches!(item, TurnItem::ToolResult { .. }));
        let id = ModelResponseId::generate();
        let message = MessageId::generate();
        let mut events = VecDeque::from([
            AgentEvent::ResponseStarted {
                response_id: id.clone(),
                assistant_message_id: message.clone(),
            },
            AgentEvent::Usage {
                usage: Usage::new(Some(2), Some(1), Some(3)),
            },
        ]);
        if !calls {
            events.push_back(AgentEvent::TextDelta {
                text: "finished".into(),
            });
        }
        Ok(Box::new(SyntheticResponse {
            response: Some(ModelResponse {
                id,
                rows: vec![TranscriptRow::new(
                    message,
                    if calls {
                        TurnItem::ToolCall {
                            tool_call_id: ToolCallId::new("fixture-call"),
                            tool_name: ToolName::new("arithmetic"),
                            input: ToolArguments::new("{\"legacy\":7}"),
                        }
                    } else {
                        TurnItem::Assistant {
                            content: "finished".into(),
                        }
                    },
                )],
                continuation: None,
                complete: true,
            }),
            events,
            facts: self.facts.clone(),
            cancel: cancel.clone(),
            draining: false,
            closed: false,
        }))
    }
}
struct SyntheticResponse {
    response: Option<ModelResponse>,
    events: VecDeque<AgentEvent>,
    facts: Arc<Facts>,
    cancel: TurnCancel,
    draining: bool,
    closed: bool,
}
#[async_trait]
impl ProviderResponse for SyntheticResponse {
    async fn next(&mut self) -> Result<ResponseStep, ProviderError> {
        if self.cancel.is_cancelled() {
            return Err(ProviderError::cancelled());
        }
        if let Some(event) = self.events.pop_front() {
            return Ok(ResponseStep::Observation(event));
        }
        if self.response.is_some() && !self.draining && !self.closed {
            Ok(ResponseStep::ReadyToSeal)
        } else {
            Ok(ResponseStep::Ended)
        }
    }
    fn seal(
        &mut self,
        normalizer: &dyn ToolArgumentNormalizer,
    ) -> Result<ModelResponse, ProviderError> {
        let mut response = self.response.take().ok_or("already sealed")?;
        for row in &mut response.rows {
            if let TurnItem::ToolCall {
                tool_name, input, ..
            } = &mut row.item
            {
                let normalized = normalizer.normalize(tool_name, input)?;
                self.facts
                    .canonical
                    .lock()
                    .unwrap()
                    .push((input.clone(), normalized.clone()));
                *input = normalized;
            }
        }
        self.facts.seals.fetch_add(1, Ordering::SeqCst);
        Ok(response)
    }
    fn begin_usage_drain(&mut self) {
        self.draining = true;
        self.response = None;
        self.events
            .retain(|event| matches!(event, AgentEvent::Usage { .. }));
    }
    fn close(&mut self) {
        if !self.closed {
            self.facts.closes.fetch_add(1, Ordering::SeqCst);
            self.closed = true;
            self.events.clear();
            self.response = None;
        }
    }
}
fn configured(facts: &Arc<Facts>) -> (LlmRuntime, TurnRequest) {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(Arithmetic(facts.clone()))).unwrap();
    let tools = Arc::new(tools);
    let request = TurnRequest {
        session_id: SessionId::generate(),
        job_id: JobId::generate(),
        history: vec![TurnItem::User {
            content: MessageContent::text("compute"),
        }],
        prefix: AgentPrefix {
            tools: tools.specs(),
            ..AgentPrefix::default()
        },
        cancel: TurnCancel::new(),
        tail_state: None,
        tool_call_seal: ToolCallSealPolicy::SealAlways,
    };
    (
        LlmRuntime::new(Arc::new(SyntheticProtocol::new(facts.clone()))).with_tools(tools),
        request,
    )
}

#[tokio::test]
async fn third_protocol_obeys_both_commit_barriers_and_owns_canonical_mapping() {
    let facts = Arc::new(Facts::default());
    let (runtime, request) = configured(&facts);
    let mut run = runtime.run(request).await.unwrap();
    loop {
        if let AgentEvent::ModelResponseCompleted { response } = run.next().await.unwrap().unwrap()
        {
            assert!(
                matches!(&response.rows[0].item, TurnItem::ToolCall { input, .. } if input.parse().unwrap() == json!({"number":7}))
            );
            break;
        }
    }
    assert!(facts.executions.lock().unwrap().is_empty());
    assert_eq!(facts.opens.load(Ordering::SeqCst), 1);
    assert!(matches!(
        run.next().await.unwrap().unwrap(),
        AgentEvent::ToolResult {
            status: ToolResultStatus::Ok,
            ..
        }
    ));
    assert_eq!(
        facts.executions.lock().unwrap().as_slice(),
        &[json!({"number":7})]
    );
    assert_eq!(facts.opens.load(Ordering::SeqCst), 1);
    assert!(matches!(
        run.next().await.unwrap().unwrap(),
        AgentEvent::ResponseStarted { .. }
    ));
    assert_eq!(facts.opens.load(Ordering::SeqCst), 2);
    while let Some(event) = run.next().await {
        event.unwrap();
    }
    run.close_and_join().await.unwrap();
    assert_eq!(
        facts.canonical.lock().unwrap().as_slice(),
        &[(
            ToolArguments::new("{\"legacy\":7}"),
            ToolArguments::new("{\"number\":7}")
        )]
    );
    assert_eq!(facts.seals.load(Ordering::SeqCst), 2);
    assert_eq!(facts.closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn third_protocol_drain_and_cancel_never_seal_or_execute_tools() {
    for cancel in [false, true] {
        let facts = Arc::new(Facts::default());
        let (runtime, request) = configured(&facts);
        let token = request.cancel.clone();
        let mut run = runtime.run(request).await.unwrap();
        assert!(matches!(
            run.next().await.unwrap().unwrap(),
            AgentEvent::ResponseStarted { .. }
        ));
        if cancel {
            token.cancel();
            assert!(run.next().await.unwrap().is_err());
            assert_eq!(
                run.failure_disposition(),
                Some(FailureDisposition::Terminal)
            );
        } else {
            run.usage_drain().unwrap().begin();
            assert!(matches!(
                run.next().await.unwrap().unwrap(),
                AgentEvent::Usage { .. }
            ));
            assert!(matches!(
                run.next().await.unwrap().unwrap(),
                AgentEvent::Finished { .. }
            ));
        }
        run.close_and_join().await.unwrap();
        assert!(facts.executions.lock().unwrap().is_empty());
        assert_eq!(facts.opens.load(Ordering::SeqCst), 1);
        assert_eq!(facts.seals.load(Ordering::SeqCst), 0);
        assert_eq!(facts.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn third_protocol_oneshot_uses_the_same_response_owner_without_tools() {
    let facts = Arc::new(Facts::default());
    let (runtime, _) = configured(&facts);
    assert_eq!(runtime.complete("standalone").await.unwrap(), "finished");
    assert_eq!(facts.opens.load(Ordering::SeqCst), 1);
    assert_eq!(facts.seals.load(Ordering::SeqCst), 1);
    assert_eq!(facts.closes.load(Ordering::SeqCst), 1);
    assert!(facts.executions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_failure_category_survives_the_string_and_stream_adapter_boundaries() {
    for (kind, expected) in [
        (ProviderErrorKind::Transport, FailureDisposition::Retryable),
        (ProviderErrorKind::Http(429), FailureDisposition::Retryable),
        (ProviderErrorKind::Http(401), FailureDisposition::Terminal),
        (
            ProviderErrorKind::Continuation,
            FailureDisposition::Terminal,
        ),
    ] {
        let facts = Arc::new(Facts::default());
        let (_, mut request) = configured(&facts);
        request.prefix.tools.clear();
        let mut protocol = SyntheticProtocol::new(facts);
        protocol.failure = Some(kind);
        let runtime = LlmRuntime::new(Arc::new(protocol));
        let mut run = runtime
            .run(request)
            .await
            .unwrap()
            .map_stream(|stream| stream)
            .inspect_events(|_| {});
        assert!(run.next().await.unwrap().is_err());
        assert_eq!(run.failure_disposition(), Some(expected));
        run.close_and_join().await.unwrap();
    }
}
