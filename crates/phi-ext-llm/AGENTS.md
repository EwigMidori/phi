# AGENTS — phi-ext-llm

## Role

LLM **provider adapters** for **phi**: HTTP/SSE wire dialects → kernel
`AgentRuntime` / `AgentEvent`. Shared by products (phi-code, future hosts).

## Public surface

- `LlmConfig` / `ApiStyle` / `LlmConfigError`
- `OpenAiCompatRuntime` (`responses` | `completions`)

## Rules

- Implement only `AgentRuntime`; never invent parallel stream/progress enums
- Map provider SSE/JSON → `AgentEvent` (including `ReasoningDelta`, tools later)
- Kernel remains free of HTTP / provider loops
- Config values are adapter construction inputs; env key names may be productized
  at the composition root, but wire parsing stays here

## Forbidden

- Product turn orchestration / SendQueue ownership
- TUI / scrollback
- Permission / approval policy engines
- `da-*` imports
- Leaking provider-specific types into kernel

## Dependencies

- `phi-kernel` only (among phi crates)
