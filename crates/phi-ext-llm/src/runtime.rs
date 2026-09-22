//! Shared pull-driven Agent and standalone runtime.
use crate::{
    ImagePolicy, PreparedImages, ProviderError, ProviderImages, ProviderProtocol, ProviderResponse,
    ToolArgumentNormalizer,
};
use async_trait::async_trait;
use phi_kernel::{
    AgentEvent, AgentPrefix, AgentRun, AgentRuntime, ContentPart, MessageContent, ModelResponse,
    OneshotModel, OneshotRequest, OneshotText, SessionId, TurnCancel, TurnItem,
};
use std::sync::Arc;
mod conversation;
mod oneshot;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct LlmRuntime {
    protocol: Arc<dyn ProviderProtocol>,
    tools: Arc<phi_ext_tools::ToolRegistry>,
    tool_images: Option<Arc<dyn crate::ToolOutputImages>>,
    projector: Arc<dyn HistoryProjector>,
    images: Option<(Arc<ProviderImages>, ImagePolicy)>,
}
impl LlmRuntime {
    pub fn new(protocol: Arc<dyn ProviderProtocol>) -> Self {
        Self {
            protocol,
            tools: Arc::new(phi_ext_tools::ToolRegistry::new()),
            tool_images: None,
            projector: Arc::new(PassThrough),
            images: None,
        }
    }
    pub fn with_tools(mut self, tools: Arc<phi_ext_tools::ToolRegistry>) -> Self {
        self.tools = tools;
        self
    }
    pub fn with_tool_images(mut self, source: Arc<dyn crate::ToolOutputImages>) -> Self {
        self.tool_images = Some(source);
        self
    }
    pub fn with_projector(mut self, projector: Arc<dyn HistoryProjector>) -> Self {
        self.projector = projector;
        self
    }
    pub fn with_images(mut self, images: Arc<ProviderImages>, policy: ImagePolicy) -> Self {
        self.images = Some((images, policy));
        self
    }
    pub async fn validate_images(
        &self,
        session: &SessionId,
        history: &[TurnItem],
    ) -> Result<(), String> {
        if let Some((images, policy)) = &self.images {
            self.protocol.validate_image_transfer(policy.transfer)?;
            let projection = self.tool_materials(history)?;
            images.validate(policy, session, projection.as_ref().map_or(history, |value| value.asset_history.as_slice())).await
        } else if history.iter().any(|item| matches!(item, TurnItem::User { content } if content.parts().iter().any(|part| matches!(part, ContentPart::Image { .. })))) {
            Err("image source is not configured".into())
        } else { Ok(()) }
    }
    fn tool_materials(
        &self,
        history: &[TurnItem],
    ) -> Result<Option<crate::tool_images::ToolImageProjection>, String> {
        self.tool_images
            .as_ref()
            .map(|source| {
                crate::tool_images::ToolImageProjection::project(
                    history,
                    source.as_ref(),
                    self.images
                        .as_ref()
                        .is_some_and(|(_, policy)| policy.enabled),
                )
            })
            .transpose()
    }
    async fn open_response(
        &self,
        session: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        cancel: &TurnCancel,
    ) -> Result<Box<dyn ProviderResponse>, ProviderError> {
        let connection = self.protocol.connection();
        let projection = self.tool_materials(history)?;
        let asset_history = projection
            .as_ref()
            .map_or(history, |value| value.asset_history.as_slice());
        let mut images = match &self.images {
            Some((service, policy)) => {
                self.protocol.validate_image_transfer(policy.transfer)?;
                service
                    .prepare(connection, policy, session, asset_history, cancel)
                    .await?
            }
            None => PreparedImages::new(),
        };
        let mut repaired = false;
        loop {
            if let Some(projection) = &projection {
                images.attach_tool_outputs(projection.materials.clone());
            }
            self.protocol
                .validate_request(session, prefix, history, &images)?;
            match self
                .protocol
                .open_response(session, prefix, history, &images, cancel)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) => {
                    if !repaired && matches!(error.http_status(), Some(400 | 404)) {
                        if let Some((service, policy)) = &self.images {
                            if service.repair_missing(connection, &images, cancel).await? {
                                images = service
                                    .prepare(connection, policy, session, asset_history, cancel)
                                    .await?;
                                repaired = true;
                                continue;
                            }
                        }
                    }
                    return Err(error);
                }
            }
        }
    }
}

struct EnabledToolNormalizer<'a> {
    tools: &'a phi_ext_tools::ToolRegistry,
    enabled: &'a [phi_kernel::ToolSpec],
}
impl ToolArgumentNormalizer for EnabledToolNormalizer<'_> {
    fn normalize(
        &self,
        name: &phi_kernel::ToolName,
        input: &phi_kernel::ToolArguments,
    ) -> Result<phi_kernel::ToolArguments, String> {
        if !self.enabled.iter().any(|spec| spec.name == *name) {
            return Err("tool not enabled".into());
        }
        self.tools
            .normalize_arguments(name, input)
            .map_err(|_| "tool arguments rejected normalization".into())
    }
}
struct NoTools;
impl ToolArgumentNormalizer for NoTools {
    fn normalize(
        &self,
        _: &phi_kernel::ToolName,
        _: &phi_kernel::ToolArguments,
    ) -> Result<phi_kernel::ToolArguments, String> {
        Err("no tools enabled".into())
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
