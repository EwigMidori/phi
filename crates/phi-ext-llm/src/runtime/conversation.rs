use super::*;
use crate::{ProviderResponse, ResponseStep};
use futures::stream;
use phi_ext_tools::{ToolExecution, ToolExecutionScope};
use phi_kernel::{
    AgentEventStream, AgentRunLifecycle, ModelResponseId, ResponseUsageDrain, ToolArguments,
    ToolCallId, ToolName, TurnRequest,
};
use std::{
    collections::{HashSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};

struct RunLifecycle {
    scope: Arc<ToolExecutionScope>,
    finished: AtomicBool,
    usage_only: AtomicBool,
    failure: Arc<phi_kernel::RunFailure>,
}
impl ResponseUsageDrain for RunLifecycle {
    fn begin(&self) {
        self.usage_only.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl AgentRunLifecycle for RunLifecycle {
    async fn close_and_join(&self) -> Result<(), String> {
        if self.finished.load(Ordering::SeqCst) {
            self.scope.join().await;
        } else {
            self.scope.close_and_join().await;
        }
        Ok(())
    }
}

#[async_trait]
impl AgentRuntime for LlmRuntime {
    async fn run(&self, request: TurnRequest) -> Result<AgentRun, String> {
        let bound = self.tools.specs();
        let mut names = HashSet::new();
        for spec in &request.prefix.tools {
            if !names.insert(spec.name.clone()) {
                return Err("duplicate tool in agent prefix".into());
            }
            if !bound
                .iter()
                .any(|known| known.name == spec.name && known.parameters == spec.parameters)
            {
                return Err(format!(
                    "tool definition has no matching executable binding: {}",
                    spec.name
                ));
            }
        }
        let lifecycle = Arc::new(RunLifecycle {
            scope: Arc::new(
                self.tools
                    .scope_for(request.session_id.clone(), request.cancel.clone()),
            ),
            finished: AtomicBool::new(false),
            usage_only: AtomicBool::new(false),
            failure: Arc::new(phi_kernel::RunFailure::default()),
        });
        let conversation = ProviderConversation {
            runtime: self.clone(),
            history: self.projector.project(&request.history),
            request,
            reader: None,
            tools: VecDeque::new(),
            pending_response: None,
            response_count: 0,
            tool_count: 0,
            done: false,
            lifecycle: lifecycle.clone(),
        };
        let stream: AgentEventStream = Box::pin(stream::unfold(
            conversation,
            |mut conversation| async move {
                if conversation.done {
                    return None;
                }
                let event = conversation.next().await;
                if let Err(error) = &event {
                    conversation
                        .lifecycle
                        .failure
                        .record(error.failure_disposition());
                    conversation.done = true;
                    if let Some(mut reader) = conversation.reader.take() {
                        reader.close();
                    }
                }
                Some((event.map_err(ProviderError::into_message), conversation))
            },
        ));
        Ok(AgentRun::with_lifecycle(stream, lifecycle.clone())
            .with_failure_reporter(lifecycle.failure.clone())
            .with_usage_drain(lifecycle))
    }
}

struct PendingCall {
    response_id: ModelResponseId,
    id: ToolCallId,
    name: ToolName,
    arguments: ToolArguments,
}
struct ProviderConversation {
    runtime: LlmRuntime,
    request: TurnRequest,
    history: Vec<TurnItem>,
    reader: Option<Box<dyn ProviderResponse>>,
    tools: VecDeque<PendingCall>,
    pending_response: Option<ModelResponse>,
    response_count: usize,
    tool_count: usize,
    done: bool,
    lifecycle: Arc<RunLifecycle>,
}
impl ProviderConversation {
    async fn next(&mut self) -> Result<AgentEvent, ProviderError> {
        if self.request.cancel.is_cancelled() {
            return Err("generation cancelled".into());
        }
        if self.lifecycle.usage_only.load(Ordering::SeqCst) {
            // This branch is before the commit acknowledgement/tool loop. Even
            // already-decoded calls can never execute after usage draining begins.
            if let Some(reader) = &mut self.reader {
                reader.begin_usage_drain();
                match reader.next().await {
                    Ok(ResponseStep::Observation(event @ AgentEvent::Usage { .. })) => {
                        return Ok(event);
                    }
                    // The accepted answer is independent of optional metadata.
                    // A malformed/failed tail simply leaves counters unknown.
                    Ok(_) | Err(_) => {}
                }
            }
            if let Some(mut reader) = self.reader.take() {
                reader.close();
            }
            self.done = true;
            self.lifecycle.finished.store(true, Ordering::SeqCst);
            return Ok(AgentEvent::Finished {
                reason: Some("response usage drained".into()),
            });
        }
        // Reaching this poll acknowledges the previous response/result commit.
        if let Some(response) = self.pending_response.take() {
            let no_calls = self.tools.is_empty();
            self.history.push(TurnItem::ModelResponse { response });
            if no_calls {
                self.done = true;
                self.lifecycle.finished.store(true, Ordering::SeqCst);
                return Ok(AgentEvent::Finished {
                    reason: Some("stop".into()),
                });
            }
        }
        if let Some(call) = self.tools.pop_front() {
            if self.tool_count >= 32 {
                return Err("generation exceeded 32 tool calls".into());
            }
            self.tool_count += 1;
            let result = if self
                .request
                .prefix
                .tools
                .iter()
                .any(|spec| spec.name == call.name)
            {
                self.lifecycle
                    .scope
                    .execute(&call.name, call.arguments.as_str())
                    .await
            } else {
                ToolExecution::error(
                    "UnknownTool",
                    format!("Tool {} is not enabled for this generation", call.name),
                )
            };
            self.history.push(TurnItem::ToolResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name,
                output: result.output.clone(),
                status: result.status,
            });
            return Ok(AgentEvent::ToolResult {
                response_id: call.response_id,
                tool_call_id: call.id,
                output: result.output,
                status: result.status,
            });
        }
        if self.reader.is_none() {
            if self.response_count >= 16 {
                return Err("generation exceeded 16 model responses".into());
            }
            self.response_count += 1;
            let history = self
                .request
                .materialize_history(self.history.clone())
                .map_err(|error| error.to_string())?;
            self.reader = Some(
                self.runtime
                    .open_response(
                        &self.request.session_id,
                        &self.request.prefix,
                        &history,
                        &self.request.cancel,
                    )
                    .await?,
            );
        }
        let reader = self.reader.as_mut().expect("response reader");
        let response = match reader.next().await? {
            ResponseStep::Observation(event) => {
                if !matches!(
                    event,
                    AgentEvent::ResponseStarted { .. }
                        | AgentEvent::TextDelta { .. }
                        | AgentEvent::ReasoningDelta { .. }
                        | AgentEvent::Usage { .. }
                ) {
                    return Err(
                        "protocol emitted an effect instead of a response observation".into(),
                    );
                }
                return Ok(event);
            }
            ResponseStep::ReadyToSeal => reader.seal(&EnabledToolNormalizer {
                tools: &self.runtime.tools,
                enabled: &self.request.prefix.tools,
            })?,
            ResponseStep::Ended => return Err("provider ended without a complete response".into()),
        };
        if !response.complete {
            return Err("provider sealed an incomplete response".into());
        }
        if self.request.prefix.tools.is_empty()
            && response
                .rows
                .iter()
                .any(|row| matches!(row.item, TurnItem::ToolCall { .. }))
        {
            return Err("provider requested tools for a turn with no tools enabled".into());
        }
        for row in &response.rows {
            if let TurnItem::ToolCall {
                tool_call_id,
                tool_name,
                input,
            } = &row.item
            {
                self.tools.push_back(PendingCall {
                    response_id: response.id.clone(),
                    id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    arguments: input.clone(),
                });
            }
        }
        self.pending_response = Some(response.clone());
        self.reader.take().expect("response reader").close();
        Ok(AgentEvent::ModelResponseCompleted { response })
    }
}

impl Drop for ProviderConversation {
    fn drop(&mut self) {
        if let Some(reader) = &mut self.reader {
            reader.close();
        }
    }
}
