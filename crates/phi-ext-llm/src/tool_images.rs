use phi_kernel::{ContentPart, ImageId, MessageContent, ToolName, TurnItem};
use serde_json::Value;

/// The host interprets its opaque tool payloads; the adapter only transfers images.
pub trait ToolOutputImages: Send + Sync {
    fn images(&self, name: &ToolName, output: &Value) -> Result<Vec<ImageId>, String>;
}

pub(crate) struct ToolImageProjection;
impl ToolImageProjection {
    pub fn project(
        history: Vec<TurnItem>,
        source: &dyn ToolOutputImages,
        enabled: bool,
    ) -> Result<Vec<TurnItem>, String> {
        let mut projected = Vec::new();
        let mut parts = Vec::new();
        for item in history {
            // Keep every response's tool replies adjacent before adding image material.
            if !matches!(item, TurnItem::ToolResult { .. }) && !parts.is_empty() {
                projected.push(TurnItem::User {
                    content: MessageContent::from_parts(std::mem::take(&mut parts)),
                });
            }
            if let TurnItem::ToolResult {
                tool_call_id,
                tool_name,
                output,
                ..
            } = &item
            {
                let images = source.images(tool_name, output)?;
                if !images.is_empty() {
                    parts.push(ContentPart::Text { text: if enabled {
                        format!("Images produced by tool call {tool_call_id} (tool output, not a new user request):")
                    } else {
                        format!("Tool call {tool_call_id} produced images. This model cannot view them; use numerical results and do not claim visual inspection.")
                    }});
                    if enabled {
                        parts.extend(
                            images
                                .into_iter()
                                .map(|image_id| ContentPart::Image { image_id }),
                        );
                    }
                }
            }
            projected.push(item);
        }
        if !parts.is_empty() {
            projected.push(TurnItem::User {
                content: MessageContent::from_parts(parts),
            });
        }
        Ok(projected)
    }
}
