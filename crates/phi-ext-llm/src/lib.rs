//! # phi-ext-llm
//!
//! LLM **provider adapters** for [phi](https://github.com/ewigmidori/phi):
//! HTTP/SSE wire dialects → kernel [`AgentRuntime`] / [`AgentEvent`].
//!
//! | Area | Surface |
//! |------|---------|
//! | Config | [`LlmConfig`], [`ApiStyle`], [`ModelId`], [`ApiBase`], [`ApiKey`] |
//! | Strategy port | [`HistoryProjector`], default [`PassThrough`] |
//! | Runtime | [`OpenAiCompatRuntime`] — [`phi_kernel::AgentRuntime`] + [`phi_kernel::OneshotText`] |
//!
//! Context policy is **product-owned** (inject a projector). This crate is wire
//! mechanism only. Oneshot uses the **same** client with an empty product prefix.
//!
//! **Not in this crate:** product turn orchestration (`SessionHost` / SendQueue),
//! TUI, tool hosts, permission engines. Kernel stays free of provider loops.

#![forbid(unsafe_code)]

#[cfg(test)]
mod image_tests;
mod images;
mod tool_images;
pub use tool_images::ToolOutputImages;
mod openai_compat;
pub use images::{
    FileCacheKey, FileReference, FileReferenceCache, ImageDigest, ImageMetadata, ImagePolicy,
    ImageSource, ImageTransfer, ProviderFileId, ProviderImages,
};

pub use openai_compat::{
    ApiBase, ApiKey, ApiStyle, HistoryProjector, LlmConfig, ModelId, OpenAiCompatRuntime,
    PassThrough,
};

pub const EXT_LLM_NAME: &str = "phi-ext-llm";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
