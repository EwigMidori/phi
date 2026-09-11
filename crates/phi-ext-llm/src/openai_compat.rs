//! OpenAI-compatible wire: Chat Completions **or** Responses API → kernel events.
//!
//! **Mechanism objects** (Kay): encode + HTTP/SSE. Context policy is product-owned —
//! inject a [`HistoryProjector`]. Default is [`PassThrough`] (D3: no baked-in policy).

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use crate::images::{PreparedImage, PreparedImages};
use crate::{ImagePolicy, ProviderImages};
use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, Stream};
use phi_kernel::{
    AgentEvent, AgentEventStream, AgentPrefix, AgentRuntime, ContentPart, JobId, MessageContent,
    OneshotText, SessionId, ToolCallSealPolicy, TurnCancel, TurnItem, TurnRequest, Usage,
};
use reqwest::Client;
use serde_json::{Value, json};
use strum::{Display, EnumString};

// ── Config ─────────────────────────────────────────────────────────────────

/// Wire protocol for the HTTP adapter.
///
/// String form (strum): `responses` | `completions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumString, strum::AsRefStr)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum ApiStyle {
    /// `POST {base}/responses`
    #[default]
    Responses,
    /// `POST {base}/chat/completions`
    Completions,
}

/// Provider model id (e.g. `gpt-4o-mini`). Non-empty after trim.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ModelId(String);

impl ModelId {
    /// Reject empty / whitespace-only.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, String> {
        let s = value.as_ref().trim();
        if s.is_empty() {
            return Err("model id empty".into());
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ModelId").field(&self.0).finish()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// API root URL without a trailing slash (e.g. `https://api.openai.com/v1`).
///
/// Non-empty after trim; trailing `/` stripped. Scheme is not enforced (hosts may
/// use proxies or placeholders).
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ApiBase(String);

impl ApiBase {
    /// Trim, reject empty, strip trailing `/`.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, String> {
        let s = value.as_ref().trim().trim_end_matches('/');
        if s.is_empty() {
            return Err("api base empty".into());
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApiBase").field(&self.0).finish()
    }
}

impl fmt::Display for ApiBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for ApiBase {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Bearer credential. Non-empty after trim. [`Debug`] redacts the secret.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ApiKey(String);

impl ApiKey {
    /// Reject empty / whitespace-only.
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, String> {
        let s = value.as_ref().trim();
        if s.is_empty() {
            return Err("api key empty".into());
        }
        Ok(Self(s.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

impl AsRef<str> for ApiKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Endpoint settings for [`OpenAiCompatRuntime`].
///
/// Construct with typed fields; product hosts map env / config files → these newtypes.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_base: ApiBase,
    pub api_key: ApiKey,
    pub model: ModelId,
    pub api_style: ApiStyle,
}

impl LlmConfig {
    #[must_use]
    pub fn new(api_base: ApiBase, api_key: ApiKey, model: ModelId, api_style: ApiStyle) -> Self {
        Self {
            api_base,
            api_key,
            model,
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
    fn request_body(
        &self,
        model: &str,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<Value, String> {
        match self.style {
            ApiStyle::Completions => self.completions_body(model, prefix, history, images),
            ApiStyle::Responses => self.responses_body(model, prefix, history, images),
        }
    }

    fn completions_body(
        &self,
        model: &str,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<Value, String> {
        let mut messages = Vec::new();
        let preamble = prefix.render_preamble();
        if !preamble.trim().is_empty() {
            messages.push(json!({"role": "system", "content": preamble}));
        }
        for item in history {
            if let Some(msg) = self.encode_role_message(item, images)? {
                messages.push(msg);
            }
        }
        // `include_usage` so the final stream chunk carries provider usage.
        Ok(json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        }))
    }

    fn responses_body(
        &self,
        model: &str,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<Value, String> {
        let instructions = prefix.render_preamble();
        let mut input = Vec::new();
        for item in history {
            if let Some(msg) = self.encode_role_message(item, images)? {
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
        Ok(body)
    }

    /// Map one transcript row to a role message when this dialect can express it.
    fn encode_role_message(
        &self,
        item: &TurnItem,
        images: &PreparedImages,
    ) -> Result<Option<Value>, String> {
        Ok(match item {
            TurnItem::User { content } => {
                let mut parts = Vec::new();
                for part in content.parts() {
                    parts.push(match (self.style, part) {
                        (ApiStyle::Completions, ContentPart::Text { text }) => {
                            json!({"type":"text", "text":text})
                        }
                        (ApiStyle::Responses, ContentPart::Text { text }) => {
                            json!({"type":"input_text", "text":text})
                        }
                        (style, ContentPart::Image { image_id }) => {
                            let image = images
                                .get(image_id)
                                .ok_or("image content was not prepared")?;
                            match (style, image) {
                                (ApiStyle::Completions, PreparedImage::Inline(url)) => {
                                    json!({"type":"image_url", "image_url":{"url":url}})
                                }
                                (ApiStyle::Responses, PreparedImage::Inline(url)) => {
                                    json!({"type":"input_image", "image_url":url})
                                }
                                (ApiStyle::Completions, PreparedImage::File { reference, .. }) => {
                                    json!({"type":"file", "file_id":reference.id.as_str()})
                                }
                                (ApiStyle::Responses, PreparedImage::File { reference, .. }) => {
                                    json!({"type":"input_image", "file_id":reference.id.as_str()})
                                }
                            }
                        }
                    });
                }
                Some(json!({"role":"user", "content":parts}))
            }
            TurnItem::Assistant { content } if !content.is_empty() => {
                Some(json!({"role": "assistant", "content": content}))
            }
            TurnItem::Assistant { .. }
            | TurnItem::Reasoning { .. }
            | TurnItem::ToolCall { .. }
            | TurnItem::ToolResult { .. } => None,
        })
    }
}

// ── Runtime (orchestrates HTTP + collaborators) ────────────────────────────

/// HTTP streaming runtime: projector → wire codec → SSE reader → [`AgentEvent`].
pub struct OpenAiCompatRuntime {
    client: Client,
    config: LlmConfig,
    projector: Arc<dyn HistoryProjector>,
    codec: WireCodec,
    images: Option<(Arc<ProviderImages>, ImagePolicy)>,
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
            images: None,
        }
    }

    /// Inject product / binding history strategy (optional; replaces PassThrough).
    #[must_use]
    pub fn with_projector(mut self, projector: Arc<dyn HistoryProjector>) -> Self {
        self.projector = projector;
        self
    }

    #[must_use]
    pub fn with_images(mut self, images: Arc<ProviderImages>, policy: ImagePolicy) -> Self {
        self.images = Some((images, policy));
        self
    }

    /// Read-only host preflight before committing a new user message or editing history.
    pub async fn validate_images(
        &self,
        session: &SessionId,
        history: &[TurnItem],
    ) -> Result<(), String> {
        if let Some((images, policy)) = &self.images {
            images.validate(policy, session, history).await
        } else if history.iter().any(|item| {
            matches!(item, TurnItem::User { content }
            if content.parts().iter().any(|part| matches!(part, ContentPart::Image { .. })))
        }) {
            Err("image source is not configured".into())
        } else {
            Ok(())
        }
    }

    fn endpoint_url(&self) -> String {
        format!(
            "{}/{}",
            self.config.api_base.as_str(),
            self.codec.endpoint_path()
        )
    }
}

#[async_trait]
impl AgentRuntime for OpenAiCompatRuntime {
    async fn run(&self, request: TurnRequest) -> Result<AgentEventStream, String> {
        let history = request
            .materialize_history(self.projector.project(&request.history))
            .map_err(|e| e.to_string())?;
        let mut images = match &self.images {
            Some((service, policy)) => {
                service
                    .prepare(
                        &self.config,
                        policy,
                        &request.session_id,
                        &history,
                        &request.cancel,
                    )
                    .await?
            }
            None => PreparedImages::new(),
        };
        let url = self.endpoint_url();
        let mut repaired = false;
        let response = loop {
            let body = self.codec.request_body(
                self.config.model.as_str(),
                &request.prefix,
                &history,
                &images,
            )?;
            let body = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
            if self
                .images
                .as_ref()
                .is_some_and(|(_, policy)| body.len() > policy.max_request_bytes)
            {
                return Err("请求体超出 Provider 限制，请减少当前上下文的图片或文字".into());
            }
            let response = tokio::select! {
                () = request.cancel.cancelled() => return Err("request cancelled".into()),
                result = self.client.post(&url).bearer_auth(self.config.api_key.as_str())
                    .header("Content-Type", "application/json").body(body).send() => result.map_err(|e| format!("HTTP request failed: {e}"))?,
            };
            if response.status().is_success() {
                break response;
            }
            let status = response.status();
            if !repaired && matches!(status.as_u16(), 400 | 404) {
                if let Some((service, policy)) = &self.images {
                    if service
                        .repair_missing(&self.config, &images, &request.cancel)
                        .await?
                    {
                        images = service
                            .prepare(
                                &self.config,
                                policy,
                                &request.session_id,
                                &history,
                                &request.cancel,
                            )
                            .await?;
                        repaired = true;
                        continue;
                    }
                }
            }
            let text = response.text().await.unwrap_or_default();
            return Err(format!("API {status} ({url}): {text}"));
        };

        let reader = SseReader::from_byte_stream(
            response.bytes_stream(),
            request.cancel.clone(),
            self.config.api_style,
        );
        Ok(reader.into_event_stream())
    }
}

/// Same client as [`AgentRuntime`]: bare complete with **empty** product prefix / tools.
#[async_trait]
impl OneshotText for OpenAiCompatRuntime {
    async fn complete(&self, input: &str) -> Result<String, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("oneshot input empty".into());
        }
        // Explicit bare turn: empty AgentPrefix — never product binding materials.
        let request = TurnRequest {
            session_id: SessionId::generate(),
            job_id: JobId::generate(),
            history: vec![TurnItem::User {
                content: MessageContent::text(input),
            }],
            prefix: AgentPrefix::baseline_chat(Vec::new()),
            tool_call_seal: ToolCallSealPolicy::LeaveOpen,
            cancel: TurnCancel::new(),
            tail_state: None,
        };
        let mut stream = AgentRuntime::run(self, request).await?;
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(AgentEvent::TextDelta { text: t }) => text.push_str(&t),
                Ok(AgentEvent::Finished { .. }) => break,
                Ok(AgentEvent::Error { message }) => return Err(message),
                Err(message) => return Err(message),
                Ok(_) => {}
            }
        }
        Ok(text)
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
    /// Multiple events from one SSE data line (e.g. Usage then Finished).
    queued: std::collections::VecDeque<AgentEvent>,
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
            queued: std::collections::VecDeque::new(),
        }
    }

    fn into_event_stream(self) -> AgentEventStream {
        Box::pin(stream::unfold(self, |mut reader| async move {
            reader.next_event().await.map(|ev| (ev, reader))
        }))
    }

    async fn next_event(&mut self) -> Option<Result<AgentEvent, String>> {
        if let Some(ev) = self.queued.pop_front() {
            return Some(Ok(ev));
        }
        if self.done {
            return None;
        }
        loop {
            if let Some(ev) = self.queued.pop_front() {
                return Some(Ok(ev));
            }
            if self.cancel.is_cancelled() {
                self.done = true;
                return Some(Ok(AgentEvent::Finished {
                    reason: Some("cancelled".into()),
                }));
            }
            if let Some(event) = self.try_consume_line() {
                return Some(event);
            }
            let next = tokio::select! {
                () = self.cancel.cancelled() => {
                    self.done = true;
                    return Some(Ok(AgentEvent::Finished { reason: Some("cancelled".into()) }));
                },
                next = self.stream.next() => next,
            };
            match next {
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
                    if let Some(ev) = self.queued.pop_front() {
                        return Some(Ok(ev));
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
        if let Some(ev) = self.queued.pop_front() {
            return Some(Ok(ev));
        }
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
                Ok(events) if events.is_empty() => continue,
                Ok(mut events) => {
                    let first = events.remove(0);
                    if matches!(first, AgentEvent::Finished { .. }) {
                        self.done = true;
                    }
                    for ev in events {
                        if matches!(ev, AgentEvent::Finished { .. }) {
                            self.done = true;
                        }
                        self.queued.push_back(ev);
                    }
                    return Some(Ok(first));
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }

    /// Parse one SSE `data:` JSON object into zero or more kernel agent events.
    fn parse_data(&self, data: &str, event_name: Option<&str>) -> Result<Vec<AgentEvent>, String> {
        let v: Value =
            serde_json::from_str(data).map_err(|e| format!("invalid SSE JSON: {e}: {data}"))?;

        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("provider error");
            return Err(msg.to_owned());
        }

        let mut out = Vec::new();

        if let Some(ty) = v.get("type").and_then(|t| t.as_str()).or(event_name) {
            match ty {
                "response.output_text.delta" | "response.text.delta" => {
                    if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            out.push(AgentEvent::TextDelta {
                                text: delta.to_owned(),
                            });
                        }
                    }
                    return Ok(out);
                }
                "response.reasoning_text.delta" => {
                    if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            out.push(AgentEvent::ReasoningDelta {
                                text: delta.to_owned(),
                            });
                        }
                    }
                    return Ok(out);
                }
                "response.completed" | "response.done" | "response.incomplete" => {
                    if let Some(usage) = Self::usage_from_value(&v) {
                        out.push(AgentEvent::Usage { usage });
                    }
                    out.push(AgentEvent::Finished {
                        reason: Some(ty.to_owned()),
                    });
                    return Ok(out);
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
                        // Ignore other lifecycle frames; still allow top-level usage.
                        if let Some(usage) = Self::usage_from_value(&v) {
                            out.push(AgentEvent::Usage { usage });
                        }
                        return Ok(out);
                    }
                }
            }
        }

        if matches!(self.style, ApiStyle::Completions) || v.pointer("/choices/0/delta").is_some() {
            if let Some(content) = v
                .pointer("/choices/0/delta/content")
                .and_then(|c| c.as_str())
            {
                if !content.is_empty() {
                    out.push(AgentEvent::TextDelta {
                        text: content.to_owned(),
                    });
                }
            }
            if v.pointer("/choices/0/finish_reason")
                .and_then(|f| f.as_str())
                .is_some_and(|f| !f.is_empty() && f != "null")
            {
                if let Some(usage) = Self::usage_from_value(&v) {
                    out.push(AgentEvent::Usage { usage });
                }
                out.push(AgentEvent::Finished {
                    reason: v
                        .pointer("/choices/0/finish_reason")
                        .and_then(|f| f.as_str())
                        .map(str::to_owned),
                });
                return Ok(out);
            }
            // Final usage-only chunk (empty choices + usage).
            if let Some(usage) = Self::usage_from_value(&v) {
                out.push(AgentEvent::Usage { usage });
            }
            return Ok(out);
        }

        if let Some(usage) = Self::usage_from_value(&v) {
            out.push(AgentEvent::Usage { usage });
        }
        Ok(out)
    }

    /// Map provider JSON `usage` object → kernel [`Usage`]. No local totals.
    fn usage_from_value(v: &Value) -> Option<Usage> {
        let u = v.get("usage").or_else(|| v.pointer("/response/usage"))?;
        let prompt = u
            .get("prompt_tokens")
            .or_else(|| u.get("input_tokens"))
            .and_then(Value::as_u64);
        let completion = u
            .get("completion_tokens")
            .or_else(|| u.get("output_tokens"))
            .and_then(Value::as_u64);
        let total = u.get("total_tokens").and_then(Value::as_u64);
        let cached = u
            .get("prompt_cache_hit_tokens")
            .or_else(|| u.pointer("/input_tokens_details/cached_tokens"))
            .or_else(|| u.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(Value::as_u64);
        let missed = u.get("prompt_cache_miss_tokens").and_then(Value::as_u64);
        let usage = Usage::new(prompt, completion, total).with_cache(cached, missed);
        if usage.is_empty() { None } else { Some(usage) }
    }
}

#[cfg(test)]
mod config_newtype_tests {
    use super::{ApiBase, ApiKey, ModelId};

    #[test]
    fn model_id_rejects_empty() {
        assert!(ModelId::try_new("").is_err());
        assert!(ModelId::try_new("   ").is_err());
        assert_eq!(ModelId::try_new(" gpt ").unwrap().as_str(), "gpt");
    }

    #[test]
    fn api_base_strips_trailing_slash_and_rejects_empty() {
        assert!(ApiBase::try_new("").is_err());
        assert!(ApiBase::try_new("///").is_err());
        assert_eq!(
            ApiBase::try_new("https://api.openai.com/v1/")
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn api_key_rejects_empty_and_redacts_debug() {
        assert!(ApiKey::try_new("").is_err());
        let key = ApiKey::try_new("sk-secret").unwrap();
        assert_eq!(key.as_str(), "sk-secret");
        assert_eq!(format!("{key:?}"), "ApiKey(***)");
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn direct_runtime_stream_consumer_can_cancel_a_silent_connection() {
        let cancel = TurnCancel::new();
        let reader = SseReader::from_byte_stream(
            stream::pending::<Result<Bytes, reqwest::Error>>(),
            cancel.clone(),
            ApiStyle::Completions,
        );
        let mut events = reader.into_event_stream();
        let waiting = tokio::spawn(async move { events.next().await });
        tokio::task::yield_now().await;
        cancel.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, Some(Ok(AgentEvent::Finished { reason: Some(reason) })) if reason == "cancelled")
        );
    }
}
