//! OpenAI-compatible adapter: Completions and Responses retain their existing wire semantics.
use crate::{
    ApiStyle, AuthMode, HttpConnection, LlmConfig, PreparedImage, PreparedImages, ProviderError,
    ProviderProtocol, ProviderResponse, ReasoningConfig, ReasoningDialect,
};
use async_trait::async_trait;
use base64::Engine;
use phi_kernel::{
    AgentEvent, AgentPrefix, ContentPart, ModelResponse, SessionId, TurnCancel, TurnItem,
};
use reqwest::Client;
use serde_json::{Value, json};
mod response;
use response::SseReader;

/// OpenAI wire policy only. Agent and standalone lifecycles belong to LlmRuntime.
#[derive(Clone)]
pub struct OpenAiProtocol {
    connection: HttpConnection,
    config: LlmConfig,
    client: Client,
    codec: WireCodec,
}
impl OpenAiProtocol {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            connection: HttpConnection {
                api_base: config.api_base.clone(),
                api_key: config.api_key.clone(),
                auth_mode: AuthMode::Bearer,
            },
            codec: WireCodec::for_style(config.api_style),
            config,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("valid HTTP client settings"),
        }
    }
    pub fn with_auth(mut self, auth_mode: AuthMode) -> Self {
        self.connection.auth_mode = auth_mode;
        self
    }
    pub fn with_reasoning(mut self, reasoning: ReasoningConfig) -> Result<Self, String> {
        reasoning.validate(self.config.api_style)?;
        self.codec.reasoning = reasoning;
        Ok(self)
    }
    fn continuation_scope(&self) -> String {
        format!(
            "openai-compatible/v2/{}|{}|{}|{}",
            self.codec.reasoning.dialect.scope_name(),
            self.config.api_base.as_str(),
            self.config.api_style,
            self.config.model.as_str()
        )
    }
    fn body(
        &self,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<Vec<u8>, String> {
        let value = self.codec.request_body(
            self.config.model.as_str(),
            prefix,
            history,
            images,
            &self.continuation_scope(),
        )?;
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        images.validate_request_bytes(bytes.len())?;
        Ok(bytes)
    }
}
#[async_trait]
impl ProviderProtocol for OpenAiProtocol {
    fn connection(&self) -> &HttpConnection {
        &self.connection
    }
    fn validate_image_transfer(&self, _: Option<crate::ImageTransfer>) -> Result<(), String> {
        Ok(())
    }
    fn validate_request(
        &self,
        _: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<(), String> {
        self.body(prefix, history, images).map(|_| ())
    }
    async fn open_response(
        &self,
        _: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
        cancel: &TurnCancel,
    ) -> Result<Box<dyn ProviderResponse>, ProviderError> {
        let url = format!(
            "{}/{}",
            self.connection.api_base,
            self.codec.endpoint_path()
        );
        let request = self
            .connection
            .authorize(self.client.post(url))
            .header("Content-Type", "application/json")
            .body(self.body(prefix, history, images)?);
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(ProviderError::cancelled()),
            result = request.send() => result.map_err(|error| if error.is_builder() {
                ProviderError::configuration("invalid provider request URL or authentication header")
            } else { ProviderError::transport("provider HTTP request failed") })?,
        };
        if !response.status().is_success() {
            return Err(ProviderError::http(response.status().as_u16()));
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        {
            return Err(ProviderError::invalid("expected an SSE provider response"));
        }
        Ok(Box::new(SseReader::from_byte_stream(
            response.bytes_stream(),
            cancel.clone(),
            self.config.api_style,
            self.continuation_scope(),
            self.codec.reasoning.dialect,
        )))
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
    reasoning: ReasoningConfig,
}

impl WireCodec {
    fn accepts_continuation(&self, saved: &str, current: &str) -> bool {
        if saved == current {
            return true;
        }
        // Existing Responses payloads already contain complete provider items.
        // Their v1 scope predates dialect selection; preserve those files only
        // for the exact same endpoint, API style and model.
        self.style == ApiStyle::Responses
            && saved
                .strip_prefix("openai-compatible/v1|")
                .is_some_and(|identity| {
                    current
                        .split_once('|')
                        .is_some_and(|(_, now)| identity == now)
                })
    }

    fn restore_completion_metadata(
        &self,
        message: &mut Value,
        saved: &Value,
    ) -> Result<(), String> {
        let saved = saved.as_object().ok_or("invalid completion continuation")?;
        if self.reasoning.dialect == ReasoningDialect::OpenRouter {
            if let Some(details) = saved.get("reasoning_details") {
                let details_array = details
                    .as_array()
                    .ok_or("invalid reasoning details continuation")?;
                if details_array.iter().any(|value| {
                    !value.is_object() || value.get("type").and_then(Value::as_str).is_none()
                }) {
                    return Err("invalid reasoning detail continuation".into());
                }
                message["reasoning_details"] = details.clone();
            }
            if let Some(reasoning) = saved.get("reasoning") {
                if !reasoning.is_string() {
                    return Err("invalid reasoning continuation".into());
                }
                message["reasoning"] = reasoning.clone();
            }
        }
        if self.reasoning.dialect == ReasoningDialect::Gemini {
            if let Some(extra) = saved.get("extra_content") {
                Self::validate_google_signature(extra)?;
                message["extra_content"] = extra.clone();
            }
            if let Some(saved_calls) = saved.get("tool_calls") {
                let saved_calls = saved_calls
                    .as_array()
                    .ok_or("invalid signed tool continuation")?;
                let calls = message
                    .get_mut("tool_calls")
                    .and_then(Value::as_array_mut)
                    .ok_or("signed tool continuation has no matching calls")?;
                if calls.len() != saved_calls.len() {
                    return Err("signed tool continuation call count changed".into());
                }
                for (call, saved_call) in calls.iter_mut().zip(saved_calls) {
                    let arguments_match = match (
                        call.pointer("/function/arguments").and_then(Value::as_str),
                        saved_call
                            .get("canonical_arguments")
                            .or_else(|| saved_call.pointer("/function/arguments"))
                            .and_then(Value::as_str),
                    ) {
                        (Some(current), Some(original)) if current == original => true,
                        (Some(current), Some(original)) => match (
                            serde_json::from_str::<Value>(current),
                            serde_json::from_str::<Value>(original),
                        ) {
                            (Ok(current), Ok(original)) => current == original,
                            _ => false,
                        },
                        _ => false,
                    };
                    if call.get("id") != saved_call.get("id")
                        || call.pointer("/function/name") != saved_call.pointer("/function/name")
                        || !arguments_match
                    {
                        return Err("signed tool continuation no longer matches tool call".into());
                    }
                    call["function"] = saved_call["function"].clone();
                    if let Some(extra) = saved_call.get("extra_content") {
                        Self::validate_google_signature(extra)?;
                        call["extra_content"] = extra.clone();
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_google_signature(extra: &Value) -> Result<(), String> {
        if extra
            .pointer("/google/thought_signature")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err("invalid Google thought signature continuation".into());
        }
        Ok(())
    }

    fn for_style(style: ApiStyle) -> Self {
        Self {
            style,
            reasoning: ReasoningConfig::default(),
        }
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
        let mut tool_parts = Vec::new();
        for (position, item) in history.iter().enumerate() {
            if !matches!(item, TurnItem::ToolResult { .. }) && !tool_parts.is_empty() {
                self.encode_history(
                    &TurnItem::User {
                        content: phi_kernel::MessageContent::from_parts(std::mem::take(
                            &mut tool_parts,
                        )),
                    },
                    images,
                    scope,
                    &mut messages,
                )?;
            }
            self.encode_history(item, images, scope, &mut messages)?;
            if let Some(content) = images.tool_output_at(position) {
                tool_parts.extend_from_slice(content.parts());
            }
        }
        if !tool_parts.is_empty() {
            self.encode_history(
                &TurnItem::User {
                    content: phi_kernel::MessageContent::from_parts(tool_parts),
                },
                images,
                scope,
                &mut messages,
            )?;
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
        self.reasoning.apply(self.style, &mut body)?;
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
                    if self.accepts_continuation(&continuation.scope, scope) && self.style==ApiStyle::Responses {
                        let items=continuation.payload.as_array().ok_or("invalid persisted continuation")?;
                        out.extend(self.project_continuation(response, items)?);return Ok(());
                    }
                }
                if self.style==ApiStyle::Completions {
                    let mut text=String::new();let mut calls=Vec::new();let mut reasoning=String::new();
                    for row in &response.rows {match &row.item{
                        TurnItem::Assistant{content}=>text.push_str(content),
                        TurnItem::ToolCall{tool_call_id,tool_name,input}=>calls.push(json!({"id":tool_call_id.as_str(),"type":"function","function":{"name":tool_name.as_str(),"arguments":input.as_str()}})),
                        TurnItem::Reasoning{content}=>reasoning.push_str(content),
                        _=>return Err("invalid response group member".into()),
                    }}
                    let mut message=json!({"role":"assistant","content":text});
                    if self.reasoning.replay_reasoning() && (!reasoning.is_empty() || !calls.is_empty()) {
                        message["reasoning_content"] = json!(reasoning);
                    }
                    if !calls.is_empty(){message["tool_calls"]=json!(calls);}
                    if let Some(continuation) = &response.continuation {
                        if self.accepts_continuation(&continuation.scope, scope) {
                            self.restore_completion_metadata(&mut message, &continuation.payload)?;
                        }
                    }
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
                                (ApiStyle::Completions, PreparedImage::Inline { mime_type, bytes }) => {
                                    json!({"type":"image_url", "image_url":{"url":format!("data:{mime_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))}})
                                }
                                (ApiStyle::Responses, PreparedImage::Inline { mime_type, bytes }) => {
                                    json!({"type":"input_image", "image_url":format!("data:{mime_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))})
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
