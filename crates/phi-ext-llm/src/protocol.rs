//! A protocol owns one response; the runtime alone owns the tool loop.
use async_trait::async_trait;
use phi_kernel::{
    AgentEvent, AgentPrefix, ModelResponse, SessionId, ToolArguments, ToolName, TurnCancel,
    TurnItem,
};
use serde::{Deserialize, Serialize};

use crate::{ApiBase, ApiKey, ImageTransfer, PreparedImages};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMode {
    Bearer,
    GoogleApiKeyHeader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseMode {
    Streaming,
    Buffered,
}

#[derive(Clone, Debug)]
pub struct HttpConnection {
    pub api_base: ApiBase,
    pub api_key: ApiKey,
    pub auth_mode: AuthMode,
}
impl HttpConnection {
    pub fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.auth_mode {
            AuthMode::Bearer => request.bearer_auth(self.api_key.as_str()),
            AuthMode::GoogleApiKeyHeader => request.header("x-goog-api-key", self.api_key.as_str()),
        }
    }
}

/// Failure categories remain typed until the kernel String boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Transport,
    Http(u16),
    Configuration,
    Protocol,
    Blocked,
    Truncated,
    Continuation,
    Cancelled,
}
#[derive(Debug)]
pub struct ProviderError {
    kind: ProviderErrorKind,
    message: String,
}
impl ProviderError {
    pub fn http(status: u16) -> Self {
        Self {
            kind: ProviderErrorKind::Http(status),
            message: format!("provider HTTP {status}"),
        }
    }
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Transport,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::protocol(message)
    }
    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Protocol,
            message: message.into(),
        }
    }
    pub fn configuration(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Configuration,
            message: message.into(),
        }
    }
    pub fn blocked(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Blocked,
            message: message.into(),
        }
    }
    pub fn truncated(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Truncated,
            message: message.into(),
        }
    }
    pub fn continuation(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Continuation,
            message: message.into(),
        }
    }
    pub fn cancelled() -> Self {
        Self {
            kind: ProviderErrorKind::Cancelled,
            message: "request cancelled".into(),
        }
    }
    pub fn kind(&self) -> ProviderErrorKind {
        self.kind
    }
    pub fn http_status(&self) -> Option<u16> {
        if let ProviderErrorKind::Http(status) = self.kind {
            Some(status)
        } else {
            None
        }
    }
    pub fn failure_disposition(&self) -> phi_kernel::FailureDisposition {
        match self.kind {
            ProviderErrorKind::Transport | ProviderErrorKind::Http(408 | 429 | 500..=599) => {
                phi_kernel::FailureDisposition::Retryable
            }
            _ => phi_kernel::FailureDisposition::Terminal,
        }
    }
    pub fn into_message(self) -> String {
        self.message
    }
}
impl From<String> for ProviderError {
    fn from(value: String) -> Self {
        Self::invalid(value)
    }
}
impl From<&str> for ProviderError {
    fn from(value: &str) -> Self {
        Self::invalid(value)
    }
}
impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ProviderError {}

/// Response observations cannot perform effects or claim durable completion.
pub enum ResponseStep {
    Observation(AgentEvent),
    ReadyToSeal,
    Ended,
}

/// A read-only view of argument normalization, never an executable tool registry.
pub trait ToolArgumentNormalizer: Send + Sync {
    fn normalize(&self, name: &ToolName, original: &ToolArguments)
    -> Result<ToolArguments, String>;
}

#[async_trait]
pub trait ProviderResponse: Send {
    async fn next(&mut self) -> Result<ResponseStep, ProviderError>;
    fn seal(
        &mut self,
        normalizer: &dyn ToolArgumentNormalizer,
    ) -> Result<ModelResponse, ProviderError>;
    fn begin_usage_drain(&mut self);
    fn close(&mut self);
}

#[async_trait]
pub trait ProviderProtocol: Send + Sync {
    fn connection(&self) -> &HttpConnection;
    fn validate_request(
        &self,
        session: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
    ) -> Result<(), String>;
    async fn open_response(
        &self,
        session: &SessionId,
        prefix: &AgentPrefix,
        history: &[TurnItem],
        images: &PreparedImages,
        cancel: &TurnCancel,
    ) -> Result<Box<dyn ProviderResponse>, ProviderError>;
    fn validate_image_transfer(&self, transfer: Option<ImageTransfer>) -> Result<(), String> {
        if transfer == Some(ImageTransfer::DeepSeekFiles) {
            Err("this protocol does not support DeepSeek file references".into())
        } else {
            Ok(())
        }
    }
}
