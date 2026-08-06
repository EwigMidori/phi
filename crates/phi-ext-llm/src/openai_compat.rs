//! OpenAI-compatible wire: Chat Completions **or** Responses API → kernel events.
//!
//! **Mechanism objects** (Kay): encode + HTTP/SSE. Context policy is product-owned —
//! inject a [`HistoryProjector`]. Default is [`PassThrough`] (D3: no baked-in policy).

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, Stream};
use phi_kernel::{
    AgentEvent, AgentEventStream, AgentPrefix, AgentRuntime, TurnCancel, TurnItem, TurnRequest,
};
use reqwest::Client;
use serde_json::{Value, json};
use strum::{Display, EnumString};

// ── Config ─────────────────────────────────────────────────────────────────

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

// ── History strategy port (product owns implementations) ───────────────────

/// Product / binding **strategy** port: full transcript → model-visible slice.
///
/// The adapter only **asks** this object; it does not own policy.
pub trait HistoryProjector: Send + Sync {
    fn project(&self, history: &[TurnItem]) -> Vec<TurnItem>;
}

/// Mechanism default: leave history unfiltered.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassThrough;

impl HistoryProjector for PassThrough {
    fn project(&self, history: &[TurnItem]) -> Vec<TurnItem> {
        history.to_vec()
    }
}

// ── Wire codec (mechanism object) ──────────────────────────────────────────

/// Knows how one [`ApiStyle`] turns projected history into an HTTP JSON body.
///
/// Wire surface today: user/assistant role messages. Reasoning / tool rows that
/// remain after projection are not expressed on this dialect yet (product should
/// project them out or a future codec path will encode them).
struct WireCodec {
    style: ApiStyle,
}

impl WireCodec {
    fn for_style(style: ApiStyle) -> Self {
        Self { style }
    }

    fn endpoint_path(&self) -> &'static str {
        match self.style {
            ApiStyle::Responses => "responses",
            ApiStyle::Completions => "chat/completions",
        }
    }

    /// Build the full request body for one turn.
    fn request_body(&self, model: &str, prefix: &AgentPrefix, history: &[TurnItem]) -> Value {
        match self.style {
            ApiStyle::Completions => self.completions_body(model, prefix, history),
            ApiStyle::Responses => self.responses_body(model, prefix, history),
        }
    }

    fn completions_body(&self, model: &str, prefix: &AgentPrefix, history: &[TurnItem]) -> Value {
        let mut messages = Vec::new();
        let preamble = prefix.render_preamble();
        if !preamble.trim().is_empty() {
            messages.push(json!({"role": "system", "content": preamble}));
        }
        for item in history {
            if let Some(msg) = self.encode_role_message(item) {
                messages.push(msg);
            }
        }
        json!({
            "model": model,
            "messages": messages,
            "stream": true,
        })
    }

    fn responses_body(&self, model: &str, prefix: &AgentPrefix, history: &[TurnItem]) -> Value {
        let instructions = prefix.render_preamble();
        let mut input = Vec::new();
        for item in history {
            if let Some(msg) = self.encode_role_message(item) {
                input.push(msg);
            }
        }
        let mut body = json!({
            "model": model,
            "input": input,
            "stream": true,
        });
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        body
    }

    /// Map one transcript row to a role message when this dialect can express it.
    fn encode_role_message(&self, item: &TurnItem) -> Option<Value> {
        match item {
            TurnItem::User { content } => Some(json!({"role": "user", "content": content})),
            TurnItem::Assistant { content } if !content.is_empty() => {
                Some(json!({"role": "assistant", "content": content}))
            }
            TurnItem::Assistant { .. }
            | TurnItem::Reasoning { .. }
            | TurnItem::ToolCall { .. }
            | TurnItem::ToolResult { .. } => None,
        }
    }
}

// ── Runtime (orchestrates HTTP + collaborators) ────────────────────────────

/// HTTP streaming runtime: projector → wire codec → SSE reader → [`AgentEvent`].
pub struct OpenAiCompatRuntime {
    client: Client,
    config: LlmConfig,
    projector: Arc<dyn HistoryProjector>,
    codec: WireCodec,
}

impl OpenAiCompatRuntime {
    /// Default history policy is [`PassThrough`] — no product strategy baked in.
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        let codec = WireCodec::for_style(config.api_style);
        Self {
            client: Client::new(),
            config,
            projector: Arc::new(PassThrough),
            codec,
        }
    }

    /// Inject product / binding history strategy (optional; replaces PassThrough).
    #[must_use]
    pub fn with_projector(mut self, projector: Arc<dyn HistoryProjector>) -> Self {
        self.projector = projector;
        self
    }

    fn endpoint_url(&self) -> String {
        format!(
            "{}/{}",
            self.config.api_base,
            self.codec.endpoint_path()
        )
    }
}

#[async_trait]
impl AgentRuntime for OpenAiCompatRuntime {
    async fn run(&self, request: TurnRequest) -> Result<AgentEventStream, String> {
        let history = self.projector.project(&request.history);
        let body = self
            .codec
            .request_body(&self.config.model, &request.prefix, &history);
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

        let reader = SseReader::from_byte_stream(
            response.bytes_stream(),
            request.cancel.clone(),
            self.config.api_style,
        );
        Ok(reader.into_event_stream())
    }
}

// ── SSE reader (mechanism object) ──────────────────────────────────────────

/// Owns parse state for one provider SSE body → [`AgentEvent`] stream.
struct SseReader {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: String,
    done: bool,
    cancel: TurnCancel,
    style: ApiStyle,
    pending_event: Option<String>,
}

impl SseReader {
    fn from_byte_stream<S>(byte_stream: S, cancel: TurnCancel, style: ApiStyle) -> Self
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        Self {
            stream: Box::pin(byte_stream),
            buffer: String::new(),
            done: false,
            cancel,
            style,
            pending_event: None,
        }
    }

    fn into_event_stream(self) -> AgentEventStream {
        Box::pin(stream::unfold(self, |mut reader| async move {
            reader.next_event().await.map(|ev| (ev, reader))
        }))
    }

    async fn next_event(&mut self) -> Option<Result<AgentEvent, String>> {
        if self.done {
            return None;
        }
        loop {
            if self.cancel.is_cancelled() {
                self.done = true;
                return Some(Ok(AgentEvent::Finished {
                    reason: Some("cancelled".into()),
                }));
            }
            if let Some(event) = self.try_consume_line() {
                return Some(event);
            }
            match self.stream.next().await {
                Some(Ok(bytes)) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&bytes));
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(format!("stream read error: {e}")));
                }
                None => {
                    if let Some(event) = self.try_consume_line() {
                        return Some(event);
                    }
                    self.done = true;
                    return Some(Ok(AgentEvent::Finished {
                        reason: Some("stream_end".into()),
                    }));
                }
            }
        }
    }

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
            match self.parse_data(data, self.pending_event.as_deref()) {
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

    fn parse_data(
        &self,
        data: &str,
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
                    if ty.starts_with("response.") {
                        return Ok(None);
                    }
                }
            }
        }

        if matches!(self.style, ApiStyle::Completions) || v.pointer("/choices/0/delta").is_some() {
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
}
