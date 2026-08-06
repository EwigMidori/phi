# AGENTS — phi-ext-llm

## Role

LLM **provider adapters** for **phi**: HTTP/SSE → `AgentRuntime` / `AgentEvent`.

## Public surface

- `LlmConfig` / `ApiStyle` / `HistoryProjection`
- `OpenAiCompatRuntime`

## Rules

- Implement only `AgentRuntime`
- History → wire via explicit [`HistoryProjection`] (no silent drops)
- Env key names are **product-owned** (CLI maps `PHI_*` → `LlmConfig`)
- Kernel remains free of HTTP

## Forbidden

- Product turn / SendQueue ownership
- TUI
- `da-*` imports
