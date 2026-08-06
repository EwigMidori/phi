//! OpenAI-compatible wire: Chat Completions **or** Responses API → kernel events.
//!
//! Products construct [`LlmConfig`] (env keys are product-owned, not this crate).

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, Stream};
use phi_kernel::{AgentEvent, AgentEventStream, AgentRuntime, TurnCancel, TurnItem, TurnRequest};
use reqwest::Client;
use serde_json::{Value, json};
use strum::{Display, EnumString};

/// Wire protocol for the HTTP adapter.
///
/// String form (strum): `responses` | `completions`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumString, strum::AsRefStr,
)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum ApiStyle {
    /// `POST {base}/responses`
    #[default]
    Responses,
    /// `POST {base}/chat/completions`
    Completions,
}

/// Endpoint settings for [`OpenAiCompatRuntime`].
///
/// Construct explicitly; product binaries map env / config files → this struct.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    pub api_style: ApiStyle,
}

impl LlmConfig {
    #[must_use]
    pub fn new(
        api_base: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        api_style: ApiStyle,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            api_key: api_key.into(),
            model: model.into(),
            api_style,
        }
    }
}

/// How transcript history is projected onto the provider wire.
///
/// Explicit policy — not a silent drop. Extend when Responses re-sends CoT / tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryProjection {
    /// User + Assistant text only (safe for Chat Completions and lean Responses).
    #[default]
    ChatTextOnly,
}

impl HistoryProjection {
    fn include(self, item: &TurnItem) -> bool {
        match self {
            Self::ChatTextOnly => match item {
                TurnItem::User { .. } => true,
                TurnItem::Assistant { content } => !content.is_empty(),
                TurnItem::Reasoning { .. }
                | TurnItem::ToolCall { .. }
                | TurnItem::ToolResult { .. } => false,
            },
        }
    }
}

/// HTTP streaming runtime (Responses or Completions).
pub struct OpenAiCompatRuntime {
    client: Client,
    config: LlmConfig,
    history: HistoryProjection,
}

impl OpenAiCompatRuntime {
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
            history: HistoryProjection::default(),
        }
    }

    #[must_use]
    pub fn with_history_projection(mut self, history: HistoryProjection) -> Self {
        self.history = history;
        self
    }

    fn endpoint_url(&self) -> String {
        match self.config.api_style {
            ApiStyle::Responses => format!("{}/responses", self.config.api_base),
            ApiStyle::Completions => format!("{}/chat/completions", self.config.api_base),
        }
    }

    fn request_body(&self, request: &TurnRequest) -> Value {
        match self.config.api_style {
            ApiStyle::Completions => {
                let messages = history_to_chat_messages(request, self.history);
                json!({
                    "model": self.config.model,
                    "messages": messages,
                    "stream": true,
                })
            }
            ApiStyle::Responses => {
                let (instructions, input) = history_to_responses_input(request, self.history);
                let mut body = json!({
                    "model": self.config.model,
                    "input": input,
                    "stream": true,
                });
                if !instructions.is_empty() {
                    body["instructions"] = json!(instructions);
                }
                body
            }
        }
    }
}

#[async_trait]
impl AgentRuntime for OpenAiCompatRuntime {
    async fn run(&self, request: TurnRequest) -> Result<AgentEventStream, String> {
        let body = self.request_body(&request);
        let url = self.endpoint_url();

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API {status} ({url}): {text}"));
        }

        let byte_stream = response.bytes_stream();
        Ok(Box::pin(sse_byte_stream_to_events(
            byte_stream,
            request.cancel.clone(),
            self.config.api_style,
        )))
    }
}

/// Chat Completions message list.
fn history_to_chat_messages(request: &TurnRequest, projection: HistoryProjection) -> Vec<Value> {
    let mut messages = Vec::new();
    let preamble = request.prefix.render_preamble();
    if !preamble.trim().is_empty() {
        messages.push(json!({"role": "system", "content": preamble}));
    }
    for item in &request.history {
        if !projection.include(item) {
            continue;
        }
        match item {
            TurnItem::User { content } => {
                messages.push(json!({"role": "user", "content": content}));
            }
            TurnItem::Assistant { content } => {
                messages.push(json!({"role": "assistant", "content": content}));
            }
            TurnItem::Reasoning { .. }
            | TurnItem::ToolCall { .. }
            | TurnItem::ToolResult { .. } => {}
        }
    }
    messages
}

/// Responses API: optional `instructions` + `input` items.
fn history_to_responses_input(
    request: &TurnRequest,
    projection: HistoryProjection,
) -> (String, Vec<Value>) {
    let instructions = request.prefix.render_preamble();
    let mut input = Vec::new();
    for item in &request.history {
        if !projection.include(item) {
            continue;
        }
        match item {
            TurnItem::User { content } => {
                input.push(json!({"role": "user", "content": content}));
            }
            TurnItem::Assistant { content } => {
                input.push(json!({"role": "assistant", "content": content}));
            }
            TurnItem::Reasoning { .. }
            | TurnItem::ToolCall { .. }
            | TurnItem::ToolResult { .. } => {}
        }
    }
    (instructions, input)
}

fn sse_byte_stream_to_events<S>(
    byte_stream: S,
    cancel: TurnCancel,
    style: ApiStyle,
) -> impl Stream<Item = Result<AgentEvent, String>> + Send
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let state = SseParseState {
        stream: Box::pin(byte_stream),
        buffer: String::new(),
        done: false,
        cancel,
        style,
        pending_event: None,
    };
    stream::unfold(state, |mut state| async move {
        if state.done {
            return None;
        }
        loop {
            if state.cancel.is_cancelled() {
                state.done = true;
                return Some((
                    Ok(AgentEvent::Finished {
                        reason: Some("cancelled".into()),
                    }),
                    state,
                ));
            }
            if let Some(event) = state.try_consume_line() {
                return Some((event, state));
            }
            match state.stream.next().await {
                Some(Ok(bytes)) => {
                    state.buffer.push_str(&String::from_utf8_lossy(&bytes));
                }
                Some(Err(e)) => {
                    state.done = true;
                    return Some((Err(format!("stream read error: {e}")), state));
                }
                None => {
                    if let Some(event) = state.try_consume_line() {
                        return Some((event, state));
                    }
                    state.done = true;
                    return Some((
                        Ok(AgentEvent::Finished {
                            reason: Some("stream_end".into()),
                        }),
                        state,
                    ));
                }
            }
        }
    })
}

struct SseParseState {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: String,
    done: bool,
    cancel: TurnCancel,
    style: ApiStyle,
    /// Last `event:` field (Responses SSE often sets this).
    pending_event: Option<String>,
}

impl SseParseState {
    fn try_consume_line(&mut self) -> Option<Result<AgentEvent, String>> {
        loop {
            let idx = self.buffer.find('\n')?;
            let mut line = self.buffer[..idx].to_owned();
            self.buffer = self.buffer[idx + 1..].to_owned();
            if line.ends_with('\r') {
                line.pop();
            }
            let line = line.trim();
            if line.is_empty() {
                self.pending_event = None;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            if let Some(ev) = line.strip_prefix("event:") {
                self.pending_event = Some(ev.trim().to_owned());
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                self.done = true;
                return Some(Ok(AgentEvent::Finished {
                    reason: Some("stop".into()),
                }));
            }
            match parse_sse_data(data, self.style, self.pending_event.as_deref()) {
                Ok(Some(AgentEvent::Finished { reason })) => {
                    self.done = true;
                    return Some(Ok(AgentEvent::Finished { reason }));
                }
                Ok(Some(ev)) => return Some(Ok(ev)),
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn parse_sse_data(
    data: &str,
    style: ApiStyle,
    event_name: Option<&str>,
) -> Result<Option<AgentEvent>, String> {
    let v: Value =
        serde_json::from_str(data).map_err(|e| format!("invalid SSE JSON: {e}: {data}"))?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("provider error");
        return Err(msg.to_owned());
    }

    // Prefer explicit `type` (Responses API).
    if let Some(ty) = v.get("type").and_then(|t| t.as_str()).or(event_name) {
        match ty {
            "response.output_text.delta" | "response.text.delta" => {
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                    if delta.is_empty() {
                        return Ok(None);
                    }
                    return Ok(Some(AgentEvent::TextDelta {
                        text: delta.to_owned(),
                    }));
                }
            }
            // DeepSeek-V4-Flash streams CoT before final answer.
            "response.reasoning_text.delta" => {
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                    if delta.is_empty() {
                        return Ok(None);
                    }
                    return Ok(Some(AgentEvent::ReasoningDelta {
                        text: delta.to_owned(),
                    }));
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                return Ok(Some(AgentEvent::Finished {
                    reason: Some(ty.to_owned()),
                }));
            }
            "response.failed" => {
                let msg = v
                    .pointer("/response/error/message")
                    .or_else(|| v.pointer("/error/message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("response.failed");
                return Err(msg.to_owned());
            }
            _ => {
                // Ignore other response.* lifecycle events.
                if ty.starts_with("response.") {
                    return Ok(None);
                }
            }
        }
    }

    // Chat Completions stream shape (and some gateways that mix styles).
    if matches!(style, ApiStyle::Completions) || v.pointer("/choices/0/delta").is_some() {
        if let Some(content) = v
            .pointer("/choices/0/delta/content")
            .and_then(|c| c.as_str())
        {
            if content.is_empty() {
                return Ok(None);
            }
            return Ok(Some(AgentEvent::TextDelta {
                text: content.to_owned(),
            }));
        }
        // Finish reason present and no more content.
        if v.pointer("/choices/0/finish_reason")
            .and_then(|f| f.as_str())
            .is_some_and(|f| !f.is_empty() && f != "null")
        {
            return Ok(Some(AgentEvent::Finished {
                reason: v
                    .pointer("/choices/0/finish_reason")
                    .and_then(|f| f.as_str())
                    .map(str::to_owned),
            }));
        }
    }

    Ok(None)
}
