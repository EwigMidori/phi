//! Ordered model input and transient end-of-history material. No storage or provider IO.

use serde::{Deserialize, Serialize};

use crate::agent::TurnItem;
use crate::error::{KernelError, Result};
use crate::ids::ImageId;

/// One user-content part, in model-visible order. Images name immutable bytes
/// owned by the host; adapters resolve them without teaching the kernel IO.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentPart {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        image_id: ImageId,
    },
}

/// The single ordered user-message body, shared by transcript and turn material.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageContent(Vec<ContentPart>);

impl From<String> for MessageContent {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for MessageContent {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl MessageContent {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self(vec![ContentPart::Text { text: text.into() }])
    }

    #[must_use]
    pub fn from_parts(parts: Vec<ContentPart>) -> Self {
        Self(parts)
    }

    #[must_use]
    pub fn parts(&self) -> &[ContentPart] {
        &self.0
    }

    /// Text projection for search, titles and text-only displays; never replaces
    /// this object's complete model input.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for part in &self.0 {
            if let ContentPart::Text { text: part } = part {
                text.push_str(part);
            }
        }
        text
    }

    pub fn append_text(&mut self, text: impl Into<String>) {
        self.0.push(ContentPart::Text { text: text.into() });
    }

    #[must_use]
    pub fn has_input(&self) -> bool {
        self.0.iter().any(|part| match part {
            ContentPart::Text { text } => !text.trim().is_empty(),
            ContentPart::Image { .. } => true,
        })
    }
}

/// Transient turn material appended after the final user's ordered content.
/// Hosts decide its policy and text; this mechanism owns its placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailState {
    content: MessageContent,
}

impl TailState {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: MessageContent::text(text),
        }
    }

    #[must_use]
    pub fn new(content: MessageContent) -> Self {
        Self { content }
    }

    pub(crate) fn materialize(&self, history: &mut [TurnItem]) -> Result<()> {
        if !self.content.has_input() {
            return Ok(());
        }
        let content = history
            .iter_mut()
            .rev()
            .find_map(|item| match item {
                TurnItem::User { content } => Some(content),
                _ => None,
            })
            .ok_or_else(|| {
                KernelError::InvalidArgument("tail state requires a user message".into())
            })?;
        content.0.extend(self.content.0.iter().cloned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EmptyAgentPrefix, FixedToolCallSeal, InMemoryTranscript, JobId, SessionId,
        SourcesTurnMaterials, ToolCallSealPolicy, Transcript, TurnCancel, TurnMaterials,
    };
    use std::sync::Arc;

    fn request(history: Vec<TurnItem>, tail: TailState) -> crate::TurnRequest {
        let mut request = SourcesTurnMaterials::new(
            Arc::new(EmptyAgentPrefix),
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::LeaveOpen)),
        )
        .prepare(
            &SessionId::generate(),
            JobId::generate(),
            history,
            TurnCancel::new(),
        );
        request.tail_state = Some(tail);
        request
    }

    #[test]
    fn ordered_images_survive_transcript_and_tail_is_transient() {
        let image = ImageId::generate();
        let content = MessageContent::from_parts(vec![
            ContentPart::Text {
                text: "before".into(),
            },
            ContentPart::Image {
                image_id: image.clone(),
            },
            ContentPart::Text {
                text: "after".into(),
            },
        ]);
        let sid = SessionId::generate();
        let transcript = InMemoryTranscript::new();
        transcript.ensure_live(&sid).unwrap();
        transcript.record_user(&sid, &content).unwrap();
        let stored = transcript.load_turn_history(&sid).unwrap();
        let request = request(stored.clone(), TailState::text("tail"));
        let outbound = request
            .materialize_history(request.history.clone())
            .unwrap();
        let TurnItem::User { content: outbound } = &outbound[0] else {
            panic!("user row")
        };
        assert_eq!(&outbound.parts()[..3], content.parts());
        assert_eq!(
            outbound.parts()[3],
            ContentPart::Text {
                text: "tail".into()
            }
        );
        assert_eq!(request.history, stored);
        assert_eq!(transcript.load_turn_history(&sid).unwrap(), stored);
        assert_eq!(
            request.materialize_history(stored.clone()).unwrap()[0],
            TurnItem::User {
                content: outbound.clone()
            }
        );
    }

    #[test]
    fn pure_image_message_accepts_text_tail_after_image() {
        let image = ImageId::generate();
        let input = MessageContent::from_parts(vec![ContentPart::Image {
            image_id: image.clone(),
        }]);
        assert!(input.has_input());
        assert!(!MessageContent::text(" \n").has_input());
        assert!(!MessageContent::default().has_input());
        let req = request(
            vec![TurnItem::User { content: input }],
            TailState::text("state"),
        );
        let output = req.materialize_history(req.history.clone()).unwrap();
        let TurnItem::User { content } = &output[0] else {
            panic!("user row")
        };
        assert_eq!(
            content.parts(),
            &[
                ContentPart::Image { image_id: image },
                ContentPart::Text {
                    text: "state".into()
                },
            ]
        );
    }

    #[test]
    fn tail_targets_last_user_and_keeps_prefix_and_earlier_rows() {
        let history = vec![
            TurnItem::User {
                content: MessageContent::text("first"),
            },
            TurnItem::Assistant {
                content: "reply".into(),
            },
            TurnItem::User {
                content: MessageContent::text("last"),
            },
            TurnItem::Reasoning {
                content: "tool-loop reasoning".into(),
            },
        ];
        let req = request(
            history.clone(),
            TailState::new(MessageContent::from_parts(vec![ContentPart::Image {
                image_id: ImageId::generate(),
            }])),
        );
        let output = req.materialize_history(req.history.clone()).unwrap();
        assert_eq!(&output[..2], &history[..2]);
        assert_eq!(output[3], history[3]);
        assert_eq!(req.history, history);
        let TurnItem::User { content } = &output[2] else {
            panic!("last user")
        };
        assert!(matches!(content.parts()[1], ContentPart::Image { .. }));
    }

    #[test]
    fn empty_tail_is_noop_and_nonempty_tail_needs_user() {
        let empty = request(Vec::new(), TailState::text(""));
        assert!(empty.materialize_history(Vec::new()).unwrap().is_empty());
        let nonempty = request(Vec::new(), TailState::text("state"));
        assert!(matches!(
            nonempty.materialize_history(Vec::new()),
            Err(KernelError::InvalidArgument(_))
        ));
    }
}
