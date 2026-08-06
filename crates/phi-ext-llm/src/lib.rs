//! # phi-ext-llm
//!
//! LLM **provider adapters** for [phi](https://github.com/ewigmidori/phi):
//! HTTP/SSE wire dialects → kernel [`AgentRuntime`] / [`AgentEvent`].
//!
//! | Area | Surface |
//! |------|---------|
//! | Config | [`LlmConfig`], [`ApiStyle`], [`ModelId`], [`ApiBase`], [`ApiKey`] |
//! | Strategy port | [`HistoryProjector`], default [`PassThrough`] |
//! | Runtime | [`OpenAiCompatRuntime`] |
//!
//! Context policy is **product-owned** (inject a projector). This crate is wire
//! mechanism only.
//!
//! **Not in this crate:** product turn orchestration (`SessionHost` / SendQueue),
//! TUI, tool hosts, permission engines. Kernel stays free of provider loops.

#![forbid(unsafe_code)]

mod openai_compat;

pub use openai_compat::{
    ApiBase, ApiKey, ApiStyle, HistoryProjector, LlmConfig, ModelId, OpenAiCompatRuntime,
    PassThrough,
};

pub const EXT_LLM_NAME: &str = "phi-ext-llm";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
