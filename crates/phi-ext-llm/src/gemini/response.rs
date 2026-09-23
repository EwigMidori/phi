use std::{collections::VecDeque, pin::Pin};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use phi_kernel::{AgentEvent, MessageId, ModelResponse, ModelResponseId, TurnCancel, Usage};
use serde_json::Value;

use super::content::NativeContent;
use crate::{
    ResponseMode,
    framing::SseFramer,
    protocol::{ProviderError, ProviderResponse, ResponseStep, ToolArgumentNormalizer},
};

type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

enum Transport {
    Json {
        stream: Option<BodyStream>,
        buffer: Vec<u8>,
    },
    Sse(SseFramer),
}

impl Transport {
    async fn next(
        &mut self,
        cancel: &TurnCancel,
        draining: bool,
    ) -> Result<Option<Value>, ProviderError> {
        match self {
            Self::Sse(reader) => reader
                .next()
                .await?
                .map(|frame| {
                    // Neither an OpenAI [DONE] sentinel nor event names define completion.
                    serde_json::from_str(&frame.data).map_err(|_| "invalid Gemini SSE JSON".into())
                })
                .transpose(),
            Self::Json { stream, buffer } => {
                let Some(body) = stream else {
                    return Ok(None);
                };
                loop {
                    let chunk = tokio::select! {
                        () = cancel.cancelled() => return Err(ProviderError::cancelled()),
                        chunk = body.next() => chunk,
                    };
                    match chunk {
                        Some(Ok(bytes)) => {
                            let limit = if draining {
                                256 * 1024
                            } else {
                                32 * 1024 * 1024
                            };
                            if buffer.len().saturating_add(bytes.len()) > limit {
                                return Err("Gemini JSON response exceeds read limit".into());
                            }
                            buffer.extend_from_slice(&bytes);
                        }
                        Some(Err(_)) => {
                            return Err(ProviderError::transport(
                                "Gemini response transport interrupted",
                            ));
                        }
                        None => {
                            *stream = None;
                            let value = serde_json::from_slice(buffer)
                                .map_err(|_| "invalid or truncated Gemini JSON response")?;
                            buffer.clear();
                            return Ok(Some(value));
                        }
                    }
                }
            }
        }
    }

    fn begin_usage_drain(&mut self) {
        if let Self::Sse(reader) = self {
            reader.begin_usage_drain();
        }
    }

    fn close(&mut self) {
        match self {
            Self::Sse(reader) => reader.close(),
            Self::Json { stream, buffer } => {
                *stream = None;
                buffer.clear();
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponseState {
    Reading,
    Ready,
    Sealed,
    Draining,
    Failed,
    Ended,
}

pub(super) struct GeminiResponse {
    transport: Transport,
    cancel: TurnCancel,
    scope: String,
    required_signatures: bool,
    response_id: ModelResponseId,
    assistant_id: MessageId,
    parts: Vec<Value>,
    stop: bool,
    seen_candidate: bool,
    usage: Usage,
    usage_emitted: bool,
    state: ResponseState,
    fault: Option<ProviderError>,
    queued: VecDeque<AgentEvent>,
}

impl GeminiResponse {
    pub(super) fn new(
        response: reqwest::Response,
        mode: ResponseMode,
        cancel: TurnCancel,
        scope: String,
        required_signatures: bool,
    ) -> Self {
        let transport = match mode {
            ResponseMode::Buffered => Transport::Json {
                stream: Some(Box::pin(response.bytes_stream())),
                buffer: Vec::new(),
            },
            ResponseMode::Streaming => {
                Transport::Sse(SseFramer::new(response.bytes_stream(), cancel.clone()))
            }
        };
        let response_id = ModelResponseId::generate();
        let assistant_id = MessageId::generate();
        let queued = VecDeque::from([AgentEvent::ResponseStarted {
            response_id: response_id.clone(),
            assistant_message_id: assistant_id.clone(),
        }]);
        Self {
            transport,
            cancel,
            scope,
            required_signatures,
            response_id,
            assistant_id,
            parts: Vec::new(),
            stop: false,
            seen_candidate: false,
            usage: Usage::default(),
            usage_emitted: false,
            state: ResponseState::Reading,
            fault: None,
            queued,
        }
    }

    fn read_usage(&mut self, value: &Value) -> Result<(), String> {
        let Some(metadata) = value.get("usageMetadata").filter(|value| !value.is_null()) else {
            return Ok(());
        };
        let metadata = metadata
            .as_object()
            .ok_or("invalid Gemini usage metadata")?;
        let mut usage = self.usage.clone();
        for (key, target) in [
            ("promptTokenCount", &mut usage.prompt_tokens),
            ("candidatesTokenCount", &mut usage.completion_tokens),
            ("totalTokenCount", &mut usage.total_tokens),
            ("cachedContentTokenCount", &mut usage.cached_tokens),
        ] {
            if let Some(value) = metadata.get(key).filter(|value| !value.is_null()) {
                let value = value.as_u64().ok_or("invalid Gemini usage counter")?;
                if target.is_some_and(|previous| value < previous) {
                    return Err("Gemini cumulative usage counter decreased".into());
                }
                *target = Some(value);
            }
        }
        self.usage = usage;
        Ok(())
    }

    fn consume(&mut self, value: Value) -> Result<(), ProviderError> {
        if !value.is_object() {
            return Err("Gemini response is not an object".into());
        }
        self.read_usage(&value)?;
        if self.state == ResponseState::Draining || self.fault.is_some() {
            return Ok(());
        }
        if value.get("error").is_some_and(|value| !value.is_null()) {
            return Err("Gemini provider reported an error".into());
        }
        if let Some(reason) = value
            .pointer("/promptFeedback/blockReason")
            .filter(|value| !value.is_null())
        {
            if reason
                .as_str()
                .is_none_or(|reason| reason != "BLOCK_REASON_UNSPECIFIED")
            {
                return Err(ProviderError::blocked("Gemini blocked the input prompt"));
            }
        }
        let Some(candidates) = value.get("candidates").filter(|value| !value.is_null()) else {
            return Ok(());
        };
        let candidates = candidates.as_array().ok_or("invalid Gemini candidates")?;
        if candidates.is_empty() {
            return Ok(());
        }
        if candidates.len() != 1 {
            return Err(
                "Gemini returned multiple candidates for a single-candidate request".into(),
            );
        }
        let candidate = candidates[0]
            .as_object()
            .ok_or("invalid Gemini candidate")?;
        if let Some(index) = candidate.get("index").filter(|value| !value.is_null()) {
            if index.as_u64() != Some(0) {
                return Err("Gemini candidate identity changed".into());
            }
        }
        self.seen_candidate = true;
        if let Some(content) = candidate.get("content").filter(|value| !value.is_null()) {
            let content = content.as_object().ok_or("invalid Gemini content")?;
            if let Some(role) = content.get("role").filter(|value| !value.is_null()) {
                if role.as_str() != Some("model") {
                    return Err("unexpected Gemini output role".into());
                }
            }
            // ProtoJSON null means unset, including optional streaming subtrees.
            let parts = match content.get("parts").filter(|value| !value.is_null()) {
                Some(parts) => parts
                    .as_array()
                    .ok_or("invalid Gemini content parts")?
                    .as_slice(),
                None => &[],
            };
            for part in parts {
                NativeContent::validate_part(part)?;
                if self.stop
                    && (part
                        .get("functionCall")
                        .is_some_and(|value| !value.is_null())
                        || part
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty()))
                {
                    return Err("Gemini emitted new content after its terminal candidate".into());
                }
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    // Each stream part is additive, not a cumulative text snapshot.
                    // Raw signed parts themselves are never merged or deduplicated.
                    let event = if part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        AgentEvent::ReasoningDelta { text: text.into() }
                    } else {
                        AgentEvent::TextDelta { text: text.into() }
                    };
                    self.queued.push_back(event);
                }
                self.parts.push(part.clone());
            }
        }
        if let Some(reason) = candidate
            .get("finishReason")
            .filter(|value| !value.is_null())
        {
            self.read_finish_reason(reason)?;
        }
        Ok(())
    }

    fn read_finish_reason(&mut self, value: &Value) -> Result<(), ProviderError> {
        let reason = value.as_str().ok_or_else(|| {
            let kind = match value {
                Value::Number(_) => "number",
                Value::Bool(_) => "boolean",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
                _ => "null",
            };
            ProviderError::invalid(format!(
                "Gemini finishReason must be an enum name, received {kind}"
            ))
        })?;
        // Expose an enum token, never arbitrary provider text or the response body.
        if reason.len() > 128
            || !reason
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err("Gemini finishReason contains an invalid enum name".into());
        }
        match reason {
            "STOP" => self.stop = true,
            "FINISH_REASON_UNSPECIFIED" | "" => {}
            "MAX_TOKENS" => {
                return Err(ProviderError::truncated(
                    "Gemini response was truncated at its output limit (MAX_TOKENS)",
                ));
            }
            "SAFETY"
            | "RECITATION"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "IMAGE_RECITATION"
            | "LANGUAGE"
            | "MODEL_ARMOR" => {
                return Err(ProviderError::blocked(format!(
                    "Gemini blocked the generated response ({reason})"
                )));
            }
            "MALFORMED_FUNCTION_CALL"
            | "UNEXPECTED_TOOL_CALL"
            | "TOO_MANY_TOOL_CALLS"
            | "MISSING_THOUGHT_SIGNATURE"
            | "MALFORMED_RESPONSE"
            | "NO_IMAGE"
            | "OTHER"
            | "IMAGE_OTHER"
            | "ESCALATION" => {
                return Err(format!("Gemini stopped without a complete response ({reason})").into());
            }
            _ => {
                return Err(
                    format!("Gemini response has an unsupported finish reason: {reason}").into(),
                );
            }
        }
        Ok(())
    }

    fn finish(&mut self) {
        self.transport.close();
        if !self.usage_emitted && !self.usage.is_empty() {
            self.usage_emitted = true;
            self.queued.push_back(AgentEvent::Usage {
                usage: self.usage.clone(),
            });
        }
        if self.state == ResponseState::Draining {
            self.fault = None;
            self.state = ResponseState::Ended;
        } else if self.fault.is_some() {
            self.state = ResponseState::Failed;
        } else if self.seen_candidate && self.stop {
            self.state = ResponseState::Ready;
        } else {
            self.fault = Some("Gemini response ended without a complete stopped candidate".into());
            self.state = ResponseState::Failed;
        }
    }
}

#[async_trait]
impl ProviderResponse for GeminiResponse {
    async fn next(&mut self) -> Result<ResponseStep, ProviderError> {
        loop {
            if self.cancel.is_cancelled() {
                self.close();
                return Err(ProviderError::cancelled());
            }
            if let Some(event) = self.queued.pop_front() {
                return Ok(ResponseStep::Observation(event));
            }
            match self.state {
                ResponseState::Ready => return Ok(ResponseStep::ReadyToSeal),
                ResponseState::Sealed | ResponseState::Ended => return Ok(ResponseStep::Ended),
                ResponseState::Failed => {
                    self.state = ResponseState::Ended;
                    return Err(self
                        .fault
                        .take()
                        .unwrap_or_else(|| "Gemini response failed".into()));
                }
                ResponseState::Reading | ResponseState::Draining => {}
            }
            let draining = self.state == ResponseState::Draining;
            match self.transport.next(&self.cancel, draining).await {
                Ok(Some(value)) => {
                    if let Err(error) = self.consume(value) {
                        self.fault.get_or_insert(error);
                    }
                }
                Ok(None) => self.finish(),
                Err(error) => {
                    self.fault.get_or_insert(error);
                    self.finish();
                }
            }
        }
    }

    fn seal(
        &mut self,
        normalizer: &dyn ToolArgumentNormalizer,
    ) -> Result<ModelResponse, ProviderError> {
        if self.state != ResponseState::Ready {
            return Err("Gemini response is not ready to seal".into());
        }
        // Make failed sealing terminal as well: no retry can produce a second batch.
        self.state = ResponseState::Sealed;
        let native = NativeContent::new(self.parts.clone());
        native.validate_signatures(self.required_signatures)?;
        let (rows, continuation) = native.seal(&self.assistant_id, &self.scope, normalizer)?;
        Ok(ModelResponse {
            id: self.response_id.clone(),
            rows,
            continuation: Some(continuation),
            complete: true,
        })
    }

    fn begin_usage_drain(&mut self) {
        if matches!(self.state, ResponseState::Reading | ResponseState::Ready) {
            self.queued
                .retain(|event| matches!(event, AgentEvent::Usage { .. }));
            self.parts.clear();
            self.fault = None;
            self.state = ResponseState::Draining;
            self.transport.begin_usage_drain();
        }
    }

    fn close(&mut self) {
        self.transport.close();
        self.parts.clear();
        self.queued.clear();
        self.state = ResponseState::Ended;
    }
}
