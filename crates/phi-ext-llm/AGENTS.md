# AGENTS — phi-ext-llm

## Role

LLM **provider adapters** for **phi**: HTTP/SSE → `AgentRuntime` / `AgentEvent`.

## Public surface

- `LlmConfig` — `ApiBase` + `ApiKey` + `ModelId` + `ApiStyle` (typed; no bare config strings)
- `ApiBase` / `ApiKey` / `ModelId` — `try_new` rejects empty; `ApiBase` strips trailing `/`; `ApiKey` Debug redacted
- `ApiStyle` — `responses` \| `completions`
- `HistoryProjector` (strategy **port** only) + default `PassThrough`
- `OpenAiCompatRuntime` — mechanism object (codec + HTTP + SSE + provider/tool loop); `with_tools(Arc<ToolRegistry>)` binds executable mechanisms, while `request.prefix.tools` freezes the enabled catalog
- Private `ProviderConversation` and byte-framed `SseReader` assemble complete responses before yielding a durable commit batch
- `AgentRun` owns the stream and per-generation `ToolExecutionScope`; normal finish joins, cancel/error closes and joins
- Responses continuation is persisted with provider/protocol/model scope; a mismatch is an explicit error, never silently dropped

## Rules

- Implement `AgentRuntime` and the existing bare `OneshotText`; no second runtime path
- Tool responses complete only a provider round. Only the final response emits `Finished`; malformed/incomplete streams never execute accumulated calls
- Each batch/result yield is a pull commit barrier. No background producer may execute the next tool before the consumer advances
- Completions must consume trailing usage and group multiple tool calls in one assistant message; Responses must retain opaque reasoning items
- Tools execute serially in emitted order; disabled/unknown names and invalid arguments return Error, while duplicate call IDs are protocol errors
- Empty-tool turns, including OneshotText, reject unexpected tool calls without execution
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
