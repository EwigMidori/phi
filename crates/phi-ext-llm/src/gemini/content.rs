use std::collections::{HashMap, HashSet, VecDeque};

use base64::Engine;
use phi_kernel::{
    ContentPart, MessageId, ModelResponse, ProviderContinuation, ToolArguments, ToolCallId,
    ToolName, TranscriptRow, TurnItem,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{GeminiSignaturePolicy, REPLAY_SIGNATURE_PLACEHOLDER};

use crate::{
    images::{PreparedImage, PreparedImages},
    protocol::ToolArgumentNormalizer,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
struct RowSlot(usize);
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
struct PartPosition(usize);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    row: RowSlot,
    parts: Vec<PartPosition>,
    canonical_arguments: Option<ToolArguments>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedContent {
    version: u8,
    content: Value,
    bindings: Vec<Binding>,
}

/// Raw provider parts remain immutable; row slots survive document ID remapping.
pub(super) struct NativeContent {
    parts: Vec<Value>,
}

impl NativeContent {
    pub(super) fn new(parts: Vec<Value>) -> Self {
        Self { parts }
    }

    pub(super) fn validate_part(part: &Value) -> Result<(), String> {
        let object = part.as_object().ok_or("Gemini part is not an object")?;
        if let Some(thought) = object.get("thought").filter(|value| !value.is_null()) {
            if !thought.is_boolean() {
                return Err("invalid Gemini thought flag".into());
            }
        }
        if let Some(signature) = object
            .get("thoughtSignature")
            .filter(|value| !value.is_null())
        {
            let signature = signature
                .as_str()
                .ok_or("invalid Gemini thought signature")?;
            if signature.is_empty()
                || (signature != REPLAY_SIGNATURE_PLACEHOLDER
                    && base64::engine::general_purpose::STANDARD
                        .decode(signature)
                        .is_err())
            {
                return Err("invalid Gemini thought signature encoding".into());
            }
        }
        for key in object.keys() {
            if !matches!(
                key.as_str(),
                "text" | "thought" | "thoughtSignature" | "functionCall" | "partMetadata"
            ) {
                return Err(format!("unsupported Gemini output part: {key}"));
            }
        }
        match (
            object.get("text").filter(|value| !value.is_null()),
            object.get("functionCall").filter(|value| !value.is_null()),
        ) {
            (Some(text), None) if text.is_string() => Ok(()),
            (None, Some(call)) => {
                let call = call.as_object().ok_or("invalid Gemini function call")?;
                if call
                    .keys()
                    .any(|key| !matches!(key.as_str(), "name" | "args" | "id"))
                {
                    return Err("unsupported Gemini incremental function arguments".into());
                }
                let name = call
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Gemini function name missing")?;
                if name.trim().is_empty() {
                    return Err("Gemini function name empty".into());
                }
                if let Some(id) = call.get("id").filter(|value| !value.is_null()) {
                    if id.as_str().is_none_or(|id| id.trim().is_empty()) {
                        return Err("invalid Gemini function id".into());
                    }
                }
                if call
                    .get("args")
                    .is_some_and(|args| !args.is_null() && !args.is_object())
                {
                    return Err("Gemini function arguments must be an object".into());
                }
                // Schema-invalid object contents are preserved for the tool's reply.
                Ok(())
            }
            (None, None)
                if object
                    .get("thoughtSignature")
                    .is_some_and(|value| !value.is_null()) =>
            {
                Ok(())
            }
            _ => Err("unsupported or ambiguous Gemini output part".into()),
        }
    }

    pub(super) fn validate_signatures(&self, required: bool) -> Result<(), String> {
        if required
            && let Some(part) = self.parts.iter().find(|part| {
                part.get("functionCall")
                    .is_some_and(|value| !value.is_null())
            })
            && part.get("thoughtSignature").is_none_or(Value::is_null)
        {
            return Err(
                "Gemini current tool response is missing its required thought signature".into(),
            );
        }
        Ok(())
    }

    fn rows(&self, assistant_id: &MessageId) -> Result<(Vec<TranscriptRow>, Vec<Binding>), String> {
        let mut rows: Vec<TranscriptRow> = Vec::new();
        let mut bindings: Vec<Binding> = Vec::new();
        let mut provider_ids = HashSet::new();
        let mut assistant_used = false;
        for (position, part) in self.parts.iter().enumerate() {
            Self::validate_part(part)?;
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if text.is_empty() {
                    continue;
                }
                let thought = part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let append = rows.last_mut().and_then(|row| match &mut row.item {
                    TurnItem::Assistant { content } if !thought => Some(content),
                    TurnItem::Reasoning { content } if thought => Some(content),
                    _ => None,
                });
                if let Some(content) = append {
                    content.push_str(text);
                    bindings
                        .last_mut()
                        .expect("bound row")
                        .parts
                        .push(PartPosition(position));
                    continue;
                }
                let id = if !thought && !assistant_used {
                    assistant_used = true;
                    assistant_id.clone()
                } else {
                    MessageId::generate()
                };
                let item = if thought {
                    TurnItem::Reasoning {
                        content: text.into(),
                    }
                } else {
                    TurnItem::Assistant {
                        content: text.into(),
                    }
                };
                bindings.push(Binding {
                    row: RowSlot(rows.len()),
                    parts: vec![PartPosition(position)],
                    canonical_arguments: None,
                });
                rows.push(TranscriptRow::new(id, item));
            } else if let Some(call) = part.get("functionCall").filter(|value| !value.is_null()) {
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if !provider_ids.insert(id.to_owned()) {
                        return Err("duplicate Gemini function call id".into());
                    }
                }
                let name = call["name"]
                    .as_str()
                    .ok_or("Gemini function name missing")?;
                let arguments = call
                    .get("args")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let input = ToolArguments::from(arguments);
                bindings.push(Binding {
                    row: RowSlot(rows.len()),
                    parts: vec![PartPosition(position)],
                    canonical_arguments: Some(input.clone()),
                });
                rows.push(TranscriptRow::new(
                    MessageId::generate(),
                    TurnItem::ToolCall {
                        tool_call_id: ToolCallId::new(MessageId::generate().to_string()),
                        tool_name: ToolName::new(name),
                        input,
                    },
                ));
            }
        }
        if rows.is_empty() {
            return Err("Gemini produced no supported output".into());
        }
        Ok((rows, bindings))
    }

    pub(super) fn seal(
        &self,
        assistant_id: &MessageId,
        scope: &str,
        normalizer: &dyn ToolArgumentNormalizer,
    ) -> Result<(Vec<TranscriptRow>, ProviderContinuation), String> {
        let (mut rows, mut bindings) = self.rows(assistant_id)?;
        for binding in &mut bindings {
            if let TurnItem::ToolCall {
                tool_name, input, ..
            } = &mut rows[binding.row.0].item
            {
                if let Ok(accepted) = normalizer.normalize(tool_name, input) {
                    *input = accepted;
                }
                binding.canonical_arguments = Some(input.clone());
            }
        }
        let saved = SavedContent {
            version: 1,
            content: json!({"role": "model", "parts": self.parts}),
            bindings,
        };
        let payload =
            serde_json::to_value(saved).map_err(|_| "could not preserve Gemini continuation")?;
        Ok((
            rows,
            ProviderContinuation {
                scope: scope.into(),
                payload,
            },
        ))
    }

    fn restore(
        response: &ModelResponse,
        payload: &Value,
    ) -> Result<(Value, HashMap<ToolCallId, NativeCall>), String> {
        let saved: SavedContent =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid Gemini continuation")?;
        if saved.version != 1
            || !response.complete
            || saved.content.get("role").and_then(Value::as_str) != Some("model")
        {
            return Err("unsupported or incomplete Gemini continuation".into());
        }
        let parts = saved
            .content
            .get("parts")
            .and_then(Value::as_array)
            .ok_or("Gemini continuation parts missing")?;
        let native = Self::new(parts.clone());
        let (expected, expected_bindings) = native.rows(&MessageId::generate())?;
        if expected.len() != response.rows.len() || saved.bindings.len() != expected_bindings.len()
        {
            return Err("Gemini history projection changed response structure".into());
        }
        let mut calls = HashMap::new();
        for ((binding, expected_binding), (row, original)) in saved
            .bindings
            .iter()
            .zip(&expected_bindings)
            .zip(response.rows.iter().zip(&expected))
        {
            if binding.row != expected_binding.row || binding.parts != expected_binding.parts {
                return Err("invalid Gemini continuation row association".into());
            }
            match (&row.item, &original.item) {
                (TurnItem::Assistant { .. }, TurnItem::Assistant { .. })
                | (TurnItem::Reasoning { .. }, TurnItem::Reasoning { .. }) => {
                    if binding.canonical_arguments.is_some() {
                        return Err("invalid Gemini text binding".into());
                    }
                }
                (
                    TurnItem::ToolCall {
                        tool_call_id,
                        tool_name,
                        input,
                    },
                    TurnItem::ToolCall {
                        tool_name: original_name,
                        ..
                    },
                ) => {
                    if tool_name != original_name
                        || binding.canonical_arguments.as_ref() != Some(input)
                    {
                        return Err(
                            "Gemini signed call was modified without invalidating continuation"
                                .into(),
                        );
                    }
                    let call = &parts[binding.parts[0].0]["functionCall"];
                    if calls
                        .insert(
                            tool_call_id.clone(),
                            NativeCall {
                                name: tool_name.clone(),
                                provider_id: call
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            },
                        )
                        .is_some()
                    {
                        return Err("duplicate Gemini persisted call identity".into());
                    }
                }
                _ => return Err("Gemini history projection changed response row kind".into()),
            }
        }
        Ok((saved.content, calls))
    }
}

struct NativeCall {
    name: ToolName,
    provider_id: Option<String>,
}

pub(super) struct GeminiHistory<'a> {
    scope: String,
    signature_policy: GeminiSignaturePolicy,
    images: &'a PreparedImages,
    contents: Vec<Value>,
    pending: HashMap<ToolCallId, NativeCall>,
    order: VecDeque<ToolCallId>,
}

impl<'a> GeminiHistory<'a> {
    pub(super) fn new(
        scope: String,
        signature_policy: GeminiSignaturePolicy,
        images: &'a PreparedImages,
    ) -> Self {
        Self {
            scope,
            signature_policy,
            images,
            contents: Vec::new(),
            pending: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(super) fn encode(mut self, history: &[TurnItem]) -> Result<Vec<Value>, String> {
        let current = history
            .iter()
            .rposition(|item| matches!(item, TurnItem::User { .. }))
            .unwrap_or(0);
        for (index, item) in history.iter().enumerate() {
            self.item(item, index >= current, index)?;
        }
        if !self.pending.is_empty() {
            return Err("Gemini history contains unanswered tool calls".into());
        }
        for content in &mut self.contents {
            if content["role"] != "model" {
                continue;
            }
            let parts = content["parts"].as_array_mut().expect("encoded parts");
            // Empty SSE text padding is not an assistant message. Normalize only
            // the outbound copy, after validating persisted bindings; any extra
            // field (especially a signature) makes the original part significant.
            parts.retain(|part| {
                !part.as_object().is_some_and(|fields| {
                    fields.len() == 1 && fields.get("text").and_then(Value::as_str) == Some("")
                })
            });
            if self.signature_policy == GeminiSignaturePolicy::PlaceholderForMissing {
                let first_call = parts
                    .iter_mut()
                    .find(|part| part.get("functionCall").is_some_and(|call| !call.is_null()));
                if let Some(part) = first_call
                    && part.get("thoughtSignature").is_none_or(Value::is_null)
                {
                    part["thoughtSignature"] = json!(REPLAY_SIGNATURE_PLACEHOLDER);
                }
            }
        }
        self.contents.retain(|content| {
            content["role"] != "model"
                || !content["parts"]
                    .as_array()
                    .expect("encoded parts")
                    .is_empty()
        });
        if self.contents.is_empty() {
            return Err("Gemini input is empty".into());
        }
        Ok(self.contents)
    }

    fn register(&mut self, id: &ToolCallId, call: NativeCall) -> Result<(), String> {
        if self.pending.contains_key(id) {
            return Err("duplicate tool identity in Gemini history".into());
        }
        self.order.push_back(id.clone());
        self.pending.insert(id.clone(), call);
        Ok(())
    }

    fn append_model(&mut self, part: Value) {
        if let Some(content) = self
            .contents
            .last_mut()
            .filter(|value| value["role"] == "model")
        {
            content["parts"]
                .as_array_mut()
                .expect("model parts")
                .push(part);
        } else {
            self.contents
                .push(json!({"role": "model", "parts": [part]}));
        }
    }

    fn item(&mut self, item: &TurnItem, current: bool, history_index: usize) -> Result<(), String> {
        match item {
            TurnItem::User { content } => {
                if !self.pending.is_empty() {
                    return Err("new user content interrupts unanswered Gemini tool calls".into());
                }
                let mut parts = Vec::new();
                for part in content.parts() {
                    match part {
                        ContentPart::Text { text } => parts.push(json!({"text": text})),
                        ContentPart::Image { image_id, .. } => match self.images.get(image_id).ok_or("Gemini image was not prepared")? {
                            PreparedImage::Inline { mime_type, bytes } => parts.push(json!({"inlineData": {"mimeType": mime_type, "data": base64::engine::general_purpose::STANDARD.encode(bytes)}})),
                            PreparedImage::File { .. } => return Err("provider file references cannot be sent to Gemini native".into()),
                        },
                    }
                }
                self.contents.push(json!({"role": "user", "parts": parts}));
            }
            TurnItem::ModelResponse { response } => {
                if !self.pending.is_empty() {
                    return Err("model response precedes previous Gemini tool results".into());
                }
                if let Some(saved) = &response.continuation
                    && saved.scope == self.scope
                {
                    let (content, mut calls) = NativeContent::restore(response, &saved.payload)?;
                    NativeContent::new(
                        content["parts"]
                            .as_array()
                            .expect("validated parts")
                            .clone(),
                    )
                    .validate_signatures(
                        current && self.signature_policy == GeminiSignaturePolicy::Required,
                    )?;
                    for row in &response.rows {
                        if let TurnItem::ToolCall { tool_call_id, .. } = &row.item {
                            self.register(
                                tool_call_id,
                                calls
                                    .remove(tool_call_id)
                                    .ok_or("Gemini native call association missing")?,
                            )?;
                        }
                    }
                    self.contents.push(content);
                } else {
                    for row in &response.rows {
                        if !matches!(
                            row.item,
                            TurnItem::Assistant { .. }
                                | TurnItem::Reasoning { .. }
                                | TurnItem::ToolCall { .. }
                        ) {
                            return Err("invalid portable response group member".into());
                        }
                        self.item(&row.item, current, history_index)?;
                    }
                }
            }
            TurnItem::Assistant { content } => self.append_model(json!({"text": content})),
            TurnItem::Reasoning { content } => {
                self.append_model(json!({"text": content, "thought": true}))
            }
            TurnItem::ToolCall {
                tool_call_id,
                tool_name,
                input,
            } => {
                if current && self.signature_policy == GeminiSignaturePolicy::Required {
                    return Err("selected current tool turn has no Gemini thought signature; start a new user turn or use an isolated context".into());
                }
                let args = input.parse().map_err(
                    |_| "old tool arguments cannot be encoded as a Gemini function call",
                )?;
                if !args.is_object() {
                    return Err("old tool arguments are not a Gemini function object".into());
                }
                self.register(
                    tool_call_id,
                    NativeCall {
                        name: tool_name.clone(),
                        provider_id: None,
                    },
                )?;
                self.append_model(
                    json!({"functionCall": {"name": tool_name.as_str(), "args": args}}),
                );
            }
            TurnItem::ToolResult {
                tool_call_id,
                tool_name,
                output,
                status,
            } => {
                if self.order.front() != Some(tool_call_id) {
                    return Err("Gemini tool results do not follow emitted call order".into());
                }
                let call = self
                    .pending
                    .remove(tool_call_id)
                    .ok_or("Gemini tool result has no matching call")?;
                if call.name != *tool_name {
                    return Err("Gemini tool result name does not match its call".into());
                }
                self.order.pop_front();
                let mut result = json!({"name": tool_name.as_str(), "response": {"status": status, "output": output}});
                if let Some(id) = call.provider_id {
                    result["id"] = json!(id);
                }
                if let Some(material) = self.images.tool_output_at(history_index) {
                    let mut image_parts = Vec::new();
                    let mut notes = String::new();
                    for part in material.parts() {
                        match part {
                            ContentPart::Text { text } => {
                                if !notes.is_empty() { notes.push('\n'); }
                                notes.push_str(text);
                            }
                            ContentPart::Image { image_id } => match self.images.get(image_id).ok_or("Gemini tool image was not prepared")? {
                                PreparedImage::Inline { mime_type, bytes } => image_parts.push(json!({"inlineData": {"mimeType": mime_type, "data": base64::engine::general_purpose::STANDARD.encode(bytes)}})),
                                PreparedImage::File { .. } => return Err("provider file references cannot be sent as Gemini tool images".into()),
                            },
                        }
                    }
                    if !notes.is_empty() {
                        result["response"]["imageContext"] = json!(notes);
                    }
                    if !image_parts.is_empty() {
                        result["parts"] = json!(image_parts);
                    }
                }
                let part = json!({"functionResponse": result});
                if let Some(content) = self.contents.last_mut().filter(|content| {
                    content["role"] == "user"
                        && content["parts"].as_array().is_some_and(|parts| {
                            parts
                                .iter()
                                .all(|part| part.get("functionResponse").is_some())
                        })
                }) {
                    content["parts"]
                        .as_array_mut()
                        .expect("result parts")
                        .push(part);
                } else {
                    self.contents.push(json!({"role": "user", "parts": [part]}));
                }
            }
            TurnItem::Continuation { .. } => {
                return Err("Gemini continuation must belong to a response group".into());
            }
        }
        Ok(())
    }
}
