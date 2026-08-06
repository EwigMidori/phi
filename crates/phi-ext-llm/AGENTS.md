# AGENTS — phi-ext-llm

## Role

LLM **provider adapters** for **phi**: HTTP/SSE → `AgentRuntime` / `AgentEvent`.

## Public surface

- `LlmConfig` / `ApiStyle`
- `HistoryProjector` (strategy **port** only) + default `PassThrough`
- `OpenAiCompatRuntime` — mechanism object (codec + HTTP + SSE)

## Rules

- Implement only `AgentRuntime` (+ wire collaborators as private objects)
- **No baked-in product history policy** — default projector is PassThrough
- Products inject `HistoryProjector` (or filter before `TurnRequest`)
- Env key names are **product-owned**
- Prefer Kay objects / messages over free-function pipelines
- Map provider `usage` JSON → kernel `Usage` / `AgentEvent::Usage` (no local token estimates)
- Kernel remains free of HTTP

## Forbidden

- Product turn / SendQueue ownership
- Owning “correct” context policy (ChatTextOnly etc. belongs in product)
- TUI
- `da-*` imports
