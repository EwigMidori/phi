use super::*;
use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use phi_ext_tools::{ToolExecution, ToolExecutionScope};
use phi_kernel::{
    AgentEventStream, AgentRun, AgentRunLifecycle, MessageId, ModelResponse, ModelResponseId,
    ProviderContinuation, ResponseUsageDrain, ToolArguments, ToolCallId, ToolName, TranscriptRow,
    Usage,
};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(test)]
mod tests;

struct RunLifecycle {
    scope: Arc<ToolExecutionScope>,
    finished: AtomicBool,
    usage_only: AtomicBool,
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
impl AgentRuntime for OpenAiCompatRuntime {
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
            scope: Arc::new(self.tools.scope(request.cancel.clone())),
            finished: AtomicBool::new(false),
            usage_only: AtomicBool::new(false),
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
                if event.is_err() {
                    conversation.done = true;
                }
                Some((event, conversation))
            },
        ));
        Ok(AgentRun::with_lifecycle(stream, lifecycle.clone()).with_usage_drain(lifecycle))
    }
}

struct PendingCall {
    response_id: ModelResponseId,
    id: ToolCallId,
    name: ToolName,
    arguments: ToolArguments,
}
struct ProviderConversation {
    runtime: OpenAiCompatRuntime,
    request: TurnRequest,
    history: Vec<TurnItem>,
    reader: Option<SseReader>,
    tools: VecDeque<PendingCall>,
    pending_response: Option<ModelResponse>,
    response_count: usize,
    tool_count: usize,
    done: bool,
    lifecycle: Arc<RunLifecycle>,
}
impl ProviderConversation {
    async fn next(&mut self) -> Result<AgentEvent, String> {
        if self.request.cancel.is_cancelled() {
            return Err("generation cancelled".into());
        }
        if self.lifecycle.usage_only.load(Ordering::SeqCst) {
            // This branch is before the commit acknowledgement/tool loop. Even
            // already-decoded calls can never execute after usage draining begins.
            if let Some(reader) = &mut self.reader {
                reader.begin_usage_drain();
                match reader.next().await {
                    Ok(Some(event @ AgentEvent::Usage { .. })) => return Ok(event),
                    // The accepted answer is independent of optional metadata.
                    // A malformed/failed tail simply leaves counters unknown.
                    Ok(_) | Err(_) => {}
                }
            }
            self.reader = None;
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
            self.reader = Some(
                self.runtime
                    .open_response(&self.request, &self.history)
                    .await?,
            );
        }
        let event = self
            .reader
            .as_mut()
            .expect("response reader")
            .next()
            .await?
            .ok_or("provider ended without a complete response")?;
        if let AgentEvent::ModelResponseCompleted { response } = &event {
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
            self.reader = None;
        }
        Ok(event)
    }
}

/// Byte-framed SSE: UTF-8 is decoded only after a complete line, never per TCP chunk.
pub(super) struct SseReader {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: Vec<u8>,
    data: Vec<String>,
    event: Option<String>,
    decoder: ResponseDecoder,
    cancel: TurnCancel,
    queued: VecDeque<AgentEvent>,
    done: bool,
    usage_bytes_left: Option<usize>,
}
impl SseReader {
    fn begin_usage_drain(&mut self) {
        if self.usage_bytes_left.is_none() {
            self.usage_bytes_left = Some(256 * 1024);
            self.queued
                .retain(|event| matches!(event, AgentEvent::Usage { .. }));
        }
    }
    pub(super) fn from_byte_stream<S>(
        stream: S,
        cancel: TurnCancel,
        style: ApiStyle,
        scope: String,
    ) -> Self
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        let decoder = ResponseDecoder::new(style, scope);
        let mut queued = VecDeque::new();
        queued.push_back(AgentEvent::ResponseStarted {
            response_id: decoder.id.clone(),
            assistant_message_id: decoder.assistant_id.clone(),
        });
        Self {
            stream: Box::pin(stream),
            buffer: Vec::new(),
            data: Vec::new(),
            event: None,
            decoder,
            cancel,
            queued,
            done: false,
            usage_bytes_left: None,
        }
    }
    async fn next(&mut self) -> Result<Option<AgentEvent>, String> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Ok(Some(event));
            }
            if self.done {
                return Ok(None);
            }
            if self.usage_bytes_left == Some(0) {
                self.done = true;
                return Ok(None);
            }
            while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if let Some(remaining) = &mut self.usage_bytes_left {
                    if index + 1 > *remaining {
                        self.done = true;
                        return Ok(None);
                    }
                    *remaining -= index + 1;
                }
                let line: Vec<_> = self.buffer.drain(..=index).collect();
                let line = std::str::from_utf8(&line)
                    .map_err(|_| "invalid UTF-8 in provider stream")?
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                if line.is_empty() {
                    self.dispatch()?;
                    if !self.queued.is_empty() || self.done {
                        break;
                    }
                } else if let Some(data) = line.strip_prefix("data:") {
                    self.data
                        .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
                } else if let Some(event) = line.strip_prefix("event:") {
                    self.event = Some(event.trim().to_owned());
                }
            }
            if !self.queued.is_empty() || self.done {
                continue;
            }
            if self
                .usage_bytes_left
                .is_some_and(|remaining| self.buffer.len() >= remaining)
            {
                self.done = true;
                return Ok(None);
            }
            let next = tokio::select! {()=self.cancel.cancelled()=>return Err("request cancelled".into()),next=self.stream.next()=>next};
            match next {
                Some(Ok(bytes)) => {
                    if let Some(remaining) = self.usage_bytes_left {
                        let allowed = remaining.saturating_sub(self.buffer.len()).min(bytes.len());
                        self.buffer.extend_from_slice(&bytes[..allowed]);
                    } else {
                        self.buffer.extend(bytes);
                    }
                    if self.buffer.len() > 8 * 1024 * 1024 {
                        return Err("provider SSE frame exceeds limit".into());
                    }
                }
                Some(Err(error)) => return Err(format!("stream read error: {error}")),
                None => {
                    if !self.buffer.is_empty() {
                        return Err("provider stream ended inside an SSE frame".into());
                    }
                    if !self.data.is_empty() {
                        self.dispatch()?;
                    }
                    if self.usage_bytes_left.is_some() {
                        self.done = true;
                    } else if !self.done {
                        if self.decoder.style == ApiStyle::Completions
                            && self.decoder.finish_reason.is_some()
                        {
                            self.complete()?;
                        } else {
                            return Err("provider stream ended before normal completion".into());
                        }
                    }
                }
            }
        }
    }
    fn dispatch(&mut self) -> Result<(), String> {
        if self.data.is_empty() {
            self.event = None;
            return Ok(());
        }
        let data = std::mem::take(&mut self.data).join("\n");
        let event = self.event.take();
        if self.usage_bytes_left.is_some() {
            if data == "[DONE]" {
                self.done = true;
                return Ok(());
            }
            let value: Value = serde_json::from_str(&data)
                .map_err(|error| format!("invalid usage-tail SSE JSON: {error}"))?;
            if let Some(usage) = ResponseDecoder::usage(&value) {
                self.queued.push_back(AgentEvent::Usage { usage });
            }
            if matches!(
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .or(event.as_deref()),
                Some("response.completed" | "response.failed" | "response.incomplete" | "error")
            ) {
                self.done = true;
            }
            return Ok(());
        }
        if data == "[DONE]" {
            if self.decoder.style != ApiStyle::Completions {
                return Err("unexpected DONE in Responses stream".into());
            }
            return self.complete();
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|error| format!("invalid SSE JSON: {error}"))?;
        let terminal = self
            .decoder
            .consume(value, event.as_deref(), &mut self.queued)?;
        if terminal {
            self.complete()?;
        }
        Ok(())
    }
    fn complete(&mut self) -> Result<(), String> {
        let response = self.decoder.complete()?;
        self.queued
            .push_back(AgentEvent::ModelResponseCompleted { response });
        self.done = true;
        Ok(())
    }
}

#[derive(Default)]
struct CallFragments {
    id: String,
    name: String,
    arguments: String,
}
struct ResponseDecoder {
    style: ApiStyle,
    scope: String,
    id: ModelResponseId,
    assistant_id: MessageId,
    text: String,
    reasoning: String,
    items: BTreeMap<usize, Value>,
    calls: BTreeMap<usize, CallFragments>,
    finish_reason: Option<String>,
}
impl ResponseDecoder {
    fn new(style: ApiStyle, scope: String) -> Self {
        Self {
            style,
            scope,
            id: ModelResponseId::generate(),
            assistant_id: MessageId::generate(),
            text: String::new(),
            reasoning: String::new(),
            items: BTreeMap::new(),
            calls: BTreeMap::new(),
            finish_reason: None,
        }
    }
    fn consume(
        &mut self,
        value: Value,
        event: Option<&str>,
        out: &mut VecDeque<AgentEvent>,
    ) -> Result<bool, String> {
        if let Some(error) = value.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider error")
                .to_owned());
        }
        if let Some(usage) = Self::usage(&value) {
            out.push_back(AgentEvent::Usage { usage });
        }
        if self.style == ApiStyle::Completions {
            if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                if choices.len() > 1 {
                    return Err("multiple completion choices are unsupported".into());
                }
                if let Some(choice) = choices.first() {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(text) = delta.get("content").and_then(Value::as_str) {
                            self.text.push_str(text);
                            out.push_back(AgentEvent::TextDelta {
                                text: text.to_owned(),
                            });
                        }
                        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
                            self.reasoning.push_str(text);
                            out.push_back(AgentEvent::ReasoningDelta {
                                text: text.to_owned(),
                            });
                        }
                        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                            for call in calls {
                                let index = call
                                    .get("index")
                                    .and_then(Value::as_u64)
                                    .and_then(|index| usize::try_from(index).ok())
                                    .ok_or("invalid tool delta index")?;
                                let fragment = self.calls.entry(index).or_default();
                                if let Some(id) = call.get("id").and_then(Value::as_str) {
                                    fragment.id.push_str(id);
                                }
                                if let Some(name) =
                                    call.pointer("/function/name").and_then(Value::as_str)
                                {
                                    fragment.name.push_str(name);
                                }
                                if let Some(arguments) =
                                    call.pointer("/function/arguments").and_then(Value::as_str)
                                {
                                    fragment.arguments.push_str(arguments);
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        self.finish_reason = Some(reason.to_owned());
                    }
                }
            }
            return Ok(false);
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(event)
            .ok_or("Responses event has no type")?;
        match kind {
            "response.output_text.delta" => {
                let text = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or("text delta missing")?;
                self.text.push_str(text);
                out.push_back(AgentEvent::TextDelta {
                    text: text.to_owned(),
                });
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let text = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or("reasoning delta missing")?;
                self.reasoning.push_str(text);
                out.push_back(AgentEvent::ReasoningDelta {
                    text: text.to_owned(),
                });
            }
            "response.output_item.added" | "response.output_item.done" => {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("output item index missing")?;
                self.items.insert(
                    index,
                    value.get("item").cloned().ok_or("output item missing")?,
                );
            }
            "response.function_call_arguments.delta" => {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("tool arguments index missing")?;
                let item = self
                    .items
                    .get_mut(&index)
                    .ok_or("arguments preceded function item")?;
                let mut arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                arguments.push_str(
                    value
                        .get("delta")
                        .and_then(Value::as_str)
                        .ok_or("arguments delta missing")?,
                );
                item["arguments"] = json!(arguments);
            }
            "response.function_call_arguments.done" => {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("tool arguments index missing")?;
                let item = self
                    .items
                    .get_mut(&index)
                    .ok_or("arguments preceded function item")?;
                item["arguments"] = value
                    .get("arguments")
                    .cloned()
                    .ok_or("complete arguments missing")?;
            }
            "response.completed" => {
                if let Some(output) = value.pointer("/response/output").and_then(Value::as_array) {
                    self.items = output.iter().cloned().enumerate().collect();
                }
                self.finish_reason = Some("stop".into());
                return Ok(true);
            }
            "response.incomplete" => {
                return Err("provider response was incomplete; tools were not executed".into());
            }
            "response.failed" => {
                return Err(value
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("provider response failed")
                    .into());
            }
            _ => {}
        }
        Ok(false)
    }
    fn complete(&self) -> Result<ModelResponse, String> {
        if let Some(reason) = &self.finish_reason {
            if !matches!(reason.as_str(), "stop" | "tool_calls") {
                return Err(format!("provider response stopped abnormally: {reason}"));
            }
        }
        let mut rows = Vec::new();
        let mut continuation = None;
        let mut call_ids = HashSet::new();
        if self.style == ApiStyle::Responses && !self.items.is_empty() {
            let mut assistant_used = false;
            let mut opaque = false;
            for item in self.items.values() {
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        let mut text = String::new();
                        if let Some(parts) = item.get("content").and_then(Value::as_array) {
                            for part in parts {
                                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                                    text.push_str(
                                        part.get("text")
                                            .and_then(Value::as_str)
                                            .ok_or("output text missing")?,
                                    );
                                }
                            }
                        }
                        if !text.is_empty() {
                            let id = if assistant_used {
                                MessageId::generate()
                            } else {
                                assistant_used = true;
                                self.assistant_id.clone()
                            };
                            rows.push(TranscriptRow::new(
                                id,
                                TurnItem::Assistant { content: text },
                            ));
                        }
                    }
                    Some("reasoning") => {
                        opaque = true;
                        let mut text = String::new();
                        if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                            for part in summary {
                                if let Some(value) = part.get("text").and_then(Value::as_str) {
                                    text.push_str(value);
                                }
                            }
                        }
                        if !text.is_empty() {
                            rows.push(TranscriptRow::new(
                                MessageId::generate(),
                                TurnItem::Reasoning { content: text },
                            ));
                        }
                    }
                    Some("function_call") => {
                        let id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .ok_or("function call id missing")?;
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or("function name missing")?;
                        let arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .ok_or("function arguments missing")?;
                        Self::append_call(&mut rows, &mut call_ids, id, name, arguments)?;
                    }
                    Some(other) => return Err(format!("unsupported model output item: {other}")),
                    None => return Err("output item type missing".into()),
                }
            }
            if opaque {
                continuation = Some(ProviderContinuation {
                    scope: self.scope.clone(),
                    payload: json!(self.items.values().collect::<Vec<_>>()),
                });
            }
        } else {
            if !self.reasoning.is_empty() {
                rows.push(TranscriptRow::new(
                    MessageId::generate(),
                    TurnItem::Reasoning {
                        content: self.reasoning.clone(),
                    },
                ));
            }
            if !self.text.is_empty() {
                rows.push(TranscriptRow::new(
                    self.assistant_id.clone(),
                    TurnItem::Assistant {
                        content: self.text.clone(),
                    },
                ));
            }
            for call in self.calls.values() {
                Self::append_call(
                    &mut rows,
                    &mut call_ids,
                    &call.id,
                    &call.name,
                    &call.arguments,
                )?;
            }
        }
        if rows.is_empty() && continuation.is_none() {
            return Err("provider produced no supported output".into());
        }
        Ok(ModelResponse {
            id: self.id.clone(),
            rows,
            continuation,
            complete: true,
        })
    }
    fn append_call(
        rows: &mut Vec<TranscriptRow>,
        ids: &mut HashSet<String>,
        id: &str,
        name: &str,
        arguments: &str,
    ) -> Result<(), String> {
        if id.trim().is_empty() || name.trim().is_empty() {
            return Err("empty function correlation or name".into());
        }
        if !ids.insert(id.to_owned()) {
            return Err("duplicate tool call id in response".into());
        }
        rows.push(TranscriptRow::new(
            MessageId::generate(),
            TurnItem::ToolCall {
                tool_call_id: ToolCallId::new(id),
                tool_name: ToolName::new(name),
                input: ToolArguments::new(arguments),
            },
        ));
        Ok(())
    }
    fn usage(value: &Value) -> Option<Usage> {
        let usage = value
            .get("usage")
            .or_else(|| value.pointer("/response/usage"))?;
        let read = |a: &str, b: &str| {
            usage
                .get(a)
                .or_else(|| usage.get(b))
                .and_then(Value::as_u64)
        };
        let cached = usage
            .get("prompt_cache_hit_tokens")
            .or_else(|| usage.pointer("/input_tokens_details/cached_tokens"))
            .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64);
        let result = Usage::new(
            read("prompt_tokens", "input_tokens"),
            read("completion_tokens", "output_tokens"),
            usage.get("total_tokens").and_then(Value::as_u64),
        )
        .with_cache(
            cached,
            usage
                .get("prompt_cache_miss_tokens")
                .and_then(Value::as_u64),
        );
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}
