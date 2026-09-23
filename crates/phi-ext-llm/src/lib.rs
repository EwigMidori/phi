//! # phi-ext-llm
//!
//! LLM **provider adapters** for [phi](https://github.com/ewigmidori/phi):
//! HTTP/SSE wire dialects → kernel [`AgentRuntime`] / [`AgentEvent`].
//!
//! | Area | Surface |
//! |------|---------|
//! | Config | [`LlmConfig`], [`ApiStyle`], [`ModelId`], [`ApiBase`], [`ApiKey`] |
//! | Strategy port | [`HistoryProjector`], default [`PassThrough`] |
//! | Runtime | [`LlmRuntime`] — [`phi_kernel::AgentRuntime`] + [`phi_kernel::OneshotText`] |
//!
//! Context policy is **product-owned** (inject a projector). This crate is wire
//! mechanism only. Standalone completions bypass the agent loop and share its wire reader.
//! OneshotText adapts that single-response path without implicit instructions.
//!
//! **Not in this crate:** product turn orchestration (`SessionHost` / SendQueue),
//! TUI, tool hosts, permission engines. Kernel stays free of provider loops.

#![forbid(unsafe_code)]

#[cfg(test)]
mod image_tests;
mod images;
mod tool_images;
pub use tool_images::ToolOutputImages;
mod config;
pub(crate) mod framing;
mod gemini;
mod openai_compat;
mod protocol;
mod reasoning;
mod runtime;
pub use images::{
    FileCacheKey, FileReference, FileReferenceCache, ImageDigest, ImageMetadata, ImagePolicy,
    ImageSource, ImageTransfer, ProviderFileId, ProviderImages,
};
pub use reasoning::{ReasoningConfig, ReasoningDialect, ReasoningEffort, ReasoningMode};

pub use config::{ApiBase, ApiKey, ApiStyle, LlmConfig, ModelId};
pub use gemini::{GeminiProtocol, GeminiSignaturePolicy, GeminiThinking};
pub use images::{PreparedImage, PreparedImages};
pub use openai_compat::OpenAiProtocol;
pub use protocol::{
    AuthMode, HttpConnection, ProviderError, ProviderErrorKind, ProviderProtocol, ProviderResponse,
    ResponseMode, ResponseStep, ToolArgumentNormalizer,
};
pub use runtime::{HistoryProjector, LlmRuntime, PassThrough};

pub const EXT_LLM_NAME: &str = "phi-ext-llm";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
