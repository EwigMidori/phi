use phi_kernel::{ContentPart, ImageId, MessageContent, ToolName, TurnItem};
use serde_json::Value;

/// The host interprets its opaque tool payloads; the adapter only transfers images.
pub trait ToolOutputImages: Send + Sync {
    fn images(&self, name: &ToolName, output: &Value) -> Result<Vec<ImageId>, String>;
}

pub(crate) struct ToolImageProjection {
    pub asset_history: Vec<TurnItem>,
    pub materials: std::collections::HashMap<usize, MessageContent>,
}
impl ToolImageProjection {
    pub fn project(
        history: &[TurnItem],
        source: &dyn ToolOutputImages,
        enabled: bool,
    ) -> Result<Self, String> {
        let mut asset_history = history.to_vec();
        let mut materials = std::collections::HashMap::new();
        for (position, item) in history.iter().enumerate() {
            if let TurnItem::ToolResult {
                tool_call_id,
                tool_name,
                output,
                ..
            } = &item
            {
                let images = source.images(tool_name, output)?;
                if !images.is_empty() {
                    let mut parts = Vec::new();
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
                    let content = MessageContent::from_parts(parts);
                    // This view is for immutable asset IO only, never protocol history.
                    asset_history.push(TurnItem::User {
                        content: content.clone(),
                    });
                    materials.insert(position, content);
                }
            }
        }
        Ok(Self {
            asset_history,
            materials,
        })
    }
}
