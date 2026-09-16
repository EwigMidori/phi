//! OpenAI-compatible wire: Chat Completions **or** Responses API → kernel events.
//!
//! **Mechanism objects** (Kay): encode + HTTP/SSE. Context policy is product-owned —
//! inject a [`HistoryProjector`]. Default is [`PassThrough`] (D3: no baked-in policy).

use std::fmt;
use std::sync::Arc;

use self::conversation::SseReader;
use crate::images::{PreparedImage, PreparedImages};
use crate::{ImagePolicy, ProviderImages};
use async_trait::async_trait;
use phi_kernel::{
    AgentEvent, AgentPrefix, AgentRuntime, ContentPart, JobId, MessageContent, ModelResponse,
    OneshotText, SessionId, ToolCallSealPolicy, TurnCancel, TurnItem, TurnRequest,
};
use reqwest::Client;
use serde_json::{Value, json};
use strum::{Display, EnumString};
mod conversation;

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
/// Encodes complete response groups and their tool results without losing
/// provider continuation or completion-message tool-call grouping.
#[derive(Clone)]
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
        scope: &str,
    ) -> Result<Value, String> {
        let mut body = match self.style {
            ApiStyle::Completions => {
                json!({"model":model,"stream":true,"stream_options":{"include_usage":true}})
            }
            ApiStyle::Responses => json!({"model":model,"stream":true}),
        };
        if self.style == ApiStyle::Responses && !prefix.render_preamble().is_empty() {
            body["instructions"] = json!(prefix.render_preamble());
        }
        let mut messages = Vec::new();
        if self.style == ApiStyle::Completions && !prefix.render_preamble().trim().is_empty() {
            messages.push(json!({"role":"system","content":prefix.render_preamble()}));
        }
        for item in history {
            self.encode_history(item, images, scope, &mut messages)?;
        }
        body[if self.style == ApiStyle::Responses {
            "input"
        } else {
            "messages"
        }] = json!(messages);
        if !prefix.tools.is_empty() {
            let mut tools = Vec::new();
            for spec in &prefix.tools {
                let function = json!({"name":spec.name.as_str(),"description":spec.description,"parameters":spec.parameters.clone().unwrap_or_else(||json!({"type":"object","properties":{}})),"strict":false});
                tools.push(match self.style {
                    ApiStyle::Completions => json!({"type":"function","function":function}),
                    ApiStyle::Responses => {
                        let mut value = function;
                        value["type"] = json!("function");
                        value
                    }
                });
            }
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }
        if self.style == ApiStyle::Responses {
            body["store"] = json!(false);
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        Ok(body)
    }

    fn encode_history(
        &self,
        item: &TurnItem,
        images: &PreparedImages,
        scope: &str,
        out: &mut Vec<Value>,
    ) -> Result<(), String> {
        match item {
            TurnItem::ModelResponse{response}=>{
                if let Some(continuation)=&response.continuation {
                    if continuation.scope==scope && self.style==ApiStyle::Responses {
                        let items=continuation.payload.as_array().ok_or("invalid persisted continuation")?;
                        out.extend(self.project_continuation(response, items)?);return Ok(());
                    }
                }
                if self.style==ApiStyle::Completions {
                    let mut text=String::new();let mut calls=Vec::new();
                    for row in &response.rows {match &row.item{
                        TurnItem::Assistant{content}=>text.push_str(content),
                        TurnItem::ToolCall{tool_call_id,tool_name,input}=>calls.push(json!({"id":tool_call_id.as_str(),"type":"function","function":{"name":tool_name.as_str(),"arguments":input.as_str()}})),
                        TurnItem::Reasoning{..}=>{},
                        _=>return Err("invalid response group member".into()),
                    }}
                    let mut message=json!({"role":"assistant","content":text});
                    if !calls.is_empty(){message["tool_calls"]=json!(calls);}
                    out.push(message);
                }else{for row in &response.rows{self.encode_history(&row.item,images,scope,out)?;}}
            }
            TurnItem::ToolCall{tool_call_id,tool_name,input}=>out.push(match self.style{
                ApiStyle::Responses=>json!({"type":"function_call","call_id":tool_call_id.as_str(),"name":tool_name.as_str(),"arguments":input.as_str()}),
                ApiStyle::Completions=>json!({"role":"assistant","tool_calls":[{"id":tool_call_id.as_str(),"type":"function","function":{"name":tool_name.as_str(),"arguments":input.as_str()}}]}),
            }),
            TurnItem::ToolResult{tool_call_id,output,status,..}=>{
                let text=json!({"status":status,"output":output}).to_string();
                out.push(match self.style{ApiStyle::Responses=>json!({"type":"function_call_output","call_id":tool_call_id.as_str(),"output":text}),ApiStyle::Completions=>json!({"role":"tool","tool_call_id":tool_call_id.as_str(),"content":text})});
            }
            TurnItem::Continuation{..}=>return Err("continuation must belong to a response group".into()),
            _=>{if let Some(message)=self.encode_role_message(item,images)?{out.push(message);}}
        }
        Ok(())
    }

    /// Replay opaque provider state while honoring the host's visible-text projection.
    /// ResponseDecoder emits one Assistant row per nonempty message, in payload order.
    /// Structural projections cannot use this correspondence and must fail explicitly.
    fn project_continuation(
        &self,
        response: &ModelResponse,
        items: &[Value],
    ) -> Result<Vec<Value>, String> {
        let mut assistants = response.rows.iter().filter_map(|row| match &row.item {
            TurnItem::Assistant { content } => Some(content),
            _ => None,
        });
        let mut replay = items.to_vec();
        for item in &mut replay {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let parts = item
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or("continuation message content is missing")?;
            let mut original = String::new();
            for part in parts.iter() {
                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                    original.push_str(
                        part.get("text")
                            .and_then(Value::as_str)
                            .ok_or("continuation output text is missing")?,
                    );
                }
            }
            if original.is_empty() {
                continue;
            }
            let projected = assistants
                .next()
                .ok_or("continuation visible messages do not match assistant rows")?;
            if projected == &original {
                continue;
            }
            // Keep opaque item fields and nontext parts intact. A row owns the
            // combined visible text, so place its projection once in the first
            // output_text part and clear subsequent text fragments.
            let mut replacement = projected.as_str();
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("output_text") {
                    part["text"] = Value::String(replacement.to_owned());
                    replacement = "";
                }
            }
        }
        if assistants.next().is_some() {
            return Err("continuation visible messages do not match assistant rows".into());
        }
        Ok(replay)
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
            | TurnItem::ToolResult { .. }
            | TurnItem::ModelResponse { .. }
            | TurnItem::Continuation { .. } => None,
        })
    }
}

// ── Runtime (orchestrates HTTP + collaborators) ────────────────────────────

/// HTTP streaming runtime: projector → wire codec → SSE reader → [`AgentEvent`].
#[derive(Clone)]
pub struct OpenAiCompatRuntime {
    tool_images: Option<Arc<dyn crate::ToolOutputImages>>,
    tools: Arc<phi_ext_tools::ToolRegistry>,
    client: Client,
    config: LlmConfig,
    projector: Arc<dyn HistoryProjector>,
    codec: WireCodec,
    images: Option<(Arc<ProviderImages>, ImagePolicy)>,
}

impl OpenAiCompatRuntime {
    pub fn with_tool_images(mut self, source: Arc<dyn crate::ToolOutputImages>) -> Self {
        self.tool_images = Some(source);
        self
    }
    #[must_use]
    pub fn with_tools(mut self, tools: Arc<phi_ext_tools::ToolRegistry>) -> Self {
        self.tools = tools;
        self
    }
    fn continuation_scope(&self) -> String {
        format!(
            "openai-compatible/v1|{}|{}|{}",
            self.config.api_base.as_str(),
            self.config.api_style,
            self.config.model.as_str()
        )
    }
    /// Default history policy is [`PassThrough`] — no product strategy baked in.
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        let codec = WireCodec::for_style(config.api_style);
        Self {
            tools: Arc::new(phi_ext_tools::ToolRegistry::new()),
            tool_images: None,
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

impl OpenAiCompatRuntime {
    async fn open_response(
        &self,
        request: &TurnRequest,
        history: &[TurnItem],
    ) -> Result<SseReader, String> {
        let history = request
            .materialize_history(history.to_vec())
            .map_err(|e| e.to_string())?;
        let history = match &self.tool_images {
            Some(source) => crate::tool_images::ToolImageProjection::project(
                history,
                source.as_ref(),
                self.images
                    .as_ref()
                    .is_some_and(|(_, policy)| policy.enabled),
            )?,
            None => history,
        };
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
                &self.continuation_scope(),
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
            self.continuation_scope(),
        );
        Ok(reader)
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
        stream.close_and_join().await?;
        Ok(text)
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    #[test]
    fn configuration_rejects_empty_values_and_redacts_keys() {
        assert!(ModelId::try_new(" ").is_err());
        assert!(ApiBase::try_new("///").is_err());
        assert!(ApiKey::try_new("").is_err());
        assert_eq!(
            ApiBase::try_new("https://example.test/v1/")
                .unwrap()
                .as_str(),
            "https://example.test/v1"
        );
        let key = ApiKey::try_new("private-key").unwrap();
        assert!(!format!("{key:?}").contains("private-key"));
    }
}
