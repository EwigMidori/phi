use serde_json::Value;

use super::content::NativeContent;

/// SSE wire fragments become complete native parts before sealing or persistence.
pub(super) struct GeminiParts {
    streaming: bool,
    complete: Vec<Value>,
    pending_header: Option<Value>,
}

impl GeminiParts {
    pub(super) fn new(streaming: bool) -> Self {
        Self {
            streaming,
            complete: Vec::new(),
            pending_header: None,
        }
    }

    pub(super) fn push(&mut self, part: Value) -> Result<(), String> {
        if self.streaming && part.pointer("/functionCall/name").and_then(Value::as_str) == Some("")
        {
            let mut header = self
                .pending_header
                .take()
                .ok_or("Gemini argument fragment has no adjacent named call header")?;
            let arguments = part
                .pointer("/functionCall/args")
                .and_then(Value::as_object)
                .filter(|args| !args.is_empty())
                .ok_or("Gemini argument fragment must contain a complete nonempty object")?;
            let mut validated = part.clone();
            validated["functionCall"]["name"] = header["functionCall"]["name"].clone();
            NativeContent::validate_part(&validated)?;
            let header_id = header.pointer("/functionCall/id").and_then(Value::as_str);
            let fragment_id = part.pointer("/functionCall/id").and_then(Value::as_str);
            if header_id.is_some() && fragment_id.is_some() && header_id != fragment_id {
                return Err("Gemini argument fragment changed its call identity".into());
            }
            if header_id.is_none() && fragment_id.is_some() {
                header["functionCall"]["id"] = part["functionCall"]["id"].clone();
            }
            // This is a whole args object after an empty header, not a recursive
            // merge or string concatenation of independently meaningful inputs.
            header["functionCall"]["args"] = Value::Object(arguments.clone());
            for (key, value) in part.as_object().expect("validated part") {
                if key == "functionCall" || value.is_null() {
                    continue;
                }
                if header
                    .get(key)
                    .is_some_and(|original| !original.is_null() && original != value)
                {
                    return Err("Gemini argument fragment changed its part metadata".into());
                }
                header[key] = value.clone();
            }
            NativeContent::validate_part(&header)?;
            self.complete.push(header);
            return Ok(());
        }
        NativeContent::validate_part(&part)?;
        self.flush_header();
        let empty_args = part.get("functionCall").is_some_and(|call| {
            call.is_object()
                && call.get("args").is_none_or(|args| {
                    args.is_null() || args.as_object().is_some_and(serde_json::Map::is_empty)
                })
        });
        if self.streaming && empty_args {
            self.pending_header = Some(part);
        } else {
            self.complete.push(part);
        }
        Ok(())
    }

    fn flush_header(&mut self) {
        if let Some(part) = self.pending_header.take() {
            self.complete.push(part);
        }
    }

    pub(super) fn finish(&mut self) -> Vec<Value> {
        self.flush_header();
        std::mem::take(&mut self.complete)
    }

    pub(super) fn clear(&mut self) {
        self.complete.clear();
        self.pending_header = None;
    }
}
