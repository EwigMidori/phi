//! # phi-ext-llm
//!
//! LLM **provider adapters** for [phi](https://github.com/ewigmidori/phi):
//! HTTP/SSE wire dialects → kernel [`AgentRuntime`] / [`AgentEvent`].
//!
//! | Area | Surface |
//! |------|---------|
//! | Config | [`LlmConfig`], [`ApiStyle`], [`HistoryProjection`] |
//! | Runtime | [`OpenAiCompatRuntime`] (`responses` / `completions`) |
//!
//! **Not in this crate:** product turn orchestration (`SessionHost` / SendQueue),
//! TUI, tool hosts, permission engines. Kernel stays free of provider loops.

#![forbid(unsafe_code)]

mod openai_compat;

pub use openai_compat::{ApiStyle, HistoryProjection, LlmConfig, OpenAiCompatRuntime};

pub const EXT_LLM_NAME: &str = "phi-ext-llm";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
