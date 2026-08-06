//! OpenAI-compatible LLM adapter: Chat Completions **or** Responses API.
//!
//! Config is env-only via `PHI_*` (see [`LlmConfig::from_env`]).

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, Stream};
use phi_kernel::{AgentEvent, AgentEventStream, AgentRuntime, TurnCancel, TurnItem, TurnRequest};
use reqwest::Client;
use serde_json::{Value, json};
use thiserror::Error;

/// Wire protocol for the HTTP adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApiStyle {
    /// `POST {base}/responses` — OpenAI Responses / DeepSeek-V4-Flash default.
    #[default]
    Responses,
    /// `POST {base}/chat/completions` — classic Chat Completions.
    Completions,
}

/// Env-driven LLM endpoint settings.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    pub api_style: ApiStyle,
}

#[derive(Debug, Error)]
pub enum LlmConfigError {
    #[error("API key missing: set PHI_API_KEY to call a real model")]
    MissingApiKey,
    #[error("invalid PHI_API_STYLE `{0}` (use `responses` or `completions`)")]
    InvalidApiStyle(String),
}

impl LlmConfig {
    /// Load from environment:
    ///
    /// | Variable | Default |
    /// |----------|---------|
    /// | `PHI_API_KEY` | required |
    /// | `PHI_API_BASE` | `https://api.openai.com/v1` |
    /// | `PHI_MODEL` | `gpt-4o-mini` |
    /// | `PHI_API_STYLE` | `responses` |
    pub fn from_env() -> Result<Self, LlmConfigError> {
        let api_key = env_trim("PHI_API_KEY");
        if api_key.is_empty() {
            return Err(LlmConfigError::MissingApiKey);
        }
        let api_base = env_trim("PHI_API_BASE");
        let api_base = if api_base.is_empty() {
            "https://api.openai.com/v1".into()
        } else {
            api_base.trim_end_matches('/').to_owned()
        };
        let model = env_trim("PHI_MODEL");
        let model = if model.is_empty() {
            "gpt-4o-mini".into()
        } else {
            model
        };
        let api_style = parse_api_style(&env_trim("PHI_API_STYLE"))?;
        Ok(Self {
            api_base,
            api_key,
            model,
            api_style,
        })
    }
}

fn env_trim(key: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_default()
}

fn parse_api_style(raw: &str) -> Result<ApiStyle, LlmConfigError> {
    match raw.trim() {
        "" | "responses" => Ok(ApiStyle::Responses),
        "completions" | "chat" | "chat_completions" => Ok(ApiStyle::Completions),
        other => Err(LlmConfigError::InvalidApiStyle(other.to_owned())),
    }
}

/// HTTP streaming runtime (Responses or Completions).
pub struct OpenAiCompatRuntime {
    client: Client,
    config: LlmConfig,
}

impl OpenAiCompatRuntime {
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
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
                let messages = history_to_chat_messages(request);
                json!({
                    "model": self.config.model,
                    "messages": messages,
                    "stream": true,
                })
            }
            ApiStyle::Responses => {
                let (instructions, input) = history_to_responses_input(request);
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
fn history_to_chat_messages(request: &TurnRequest) -> Vec<Value> {
    let mut messages = Vec::new();
    let preamble = request.prefix.render_preamble();
    if !preamble.trim().is_empty() {
        messages.push(json!({"role": "system", "content": preamble}));
    }
    for item in &request.history {
        match item {
            TurnItem::User { content } => {
                messages.push(json!({"role": "user", "content": content}));
            }
            TurnItem::Assistant { content } => {
                if !content.is_empty() {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
            // Sibling reasoning rows: chat-completions has no native slot; skip
            // (Responses-style re-send is a later product policy).
            TurnItem::Reasoning { .. }
            | TurnItem::ToolCall { .. }
            | TurnItem::ToolResult { .. } => {}
        }
    }
    messages
}

/// Responses API: optional `instructions` + `input` items.
fn history_to_responses_input(request: &TurnRequest) -> (String, Vec<Value>) {
    let instructions = request.prefix.render_preamble();
    let mut input = Vec::new();
    for item in &request.history {
        match item {
            TurnItem::User { content } => {
                input.push(json!({"role": "user", "content": content}));
            }
            TurnItem::Assistant { content } => {
                if !content.is_empty() {
                    input.push(json!({"role": "assistant", "content": content}));
                }
            }
            // Keep history lean for now; encrypted/full reasoning round-trip later.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_completions_delta() {
        let raw = r#"{"choices":[{"delta":{"content":"你好"}}]}"#;
        let ev = parse_sse_data(raw, ApiStyle::Completions, None)
            .unwrap()
            .unwrap();
        match ev {
            AgentEvent::TextDelta { text } => assert_eq!(text, "你好"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_responses_delta() {
        let raw = r#"{"type":"response.output_text.delta","delta":"Hello"}"#;
        let ev = parse_sse_data(raw, ApiStyle::Responses, None)
            .unwrap()
            .unwrap();
        match ev {
            AgentEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_responses_reasoning_delta() {
        let raw = r#"{"type":"response.reasoning_text.delta","delta":"think"}"#;
        let ev = parse_sse_data(raw, ApiStyle::Responses, None)
            .unwrap()
            .unwrap();
        match ev {
            AgentEvent::ReasoningDelta { text } => assert_eq!(text, "think"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_responses_completed() {
        let raw = r#"{"type":"response.completed"}"#;
        let ev = parse_sse_data(raw, ApiStyle::Responses, None)
            .unwrap()
            .unwrap();
        assert!(matches!(ev, AgentEvent::Finished { .. }));
    }

    #[test]
    fn parse_api_style_defaults_responses() {
        assert_eq!(parse_api_style("").unwrap(), ApiStyle::Responses);
        assert_eq!(parse_api_style("responses").unwrap(), ApiStyle::Responses);
        assert_eq!(
            parse_api_style("completions").unwrap(),
            ApiStyle::Completions
        );
    }
}
