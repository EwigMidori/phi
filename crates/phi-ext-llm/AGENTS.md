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
- Responses continuation is persisted faithfully with provider/protocol/model scope. Matching Responses scope replays the opaque payload; any other provider/protocol/model encodes the portable visible rows. Persistence never drops continuation.
- Matching-scope replay applies projected Assistant text to the corresponding visible message while preserving opaque items and tool correlation. Projection must retain one Assistant row per nonempty provider message, in original order; structural mismatches are errors.

## Rules

- `OneshotModel::generate` makes one explicit multimodal request with caller instructions and cancellation. It bypasses `ProviderConversation`, history projection, tail state and tool execution. `OneshotText` is its bare text convenience adapter. Agent turns and standalone requests share the same `open_response` HTTP/image preparation and `SseReader`; never duplicate the wire stack.
- Tool responses complete only a provider round. Only the final response emits `Finished`; malformed/incomplete streams never execute accumulated calls
- Each batch/result yield is a pull commit barrier. No background producer may execute the next tool before the consumer advances
- Completions must consume trailing usage and group multiple tool calls in one assistant message; Responses must retain opaque reasoning items
- `ResponseUsageDrain` switches the existing conversation/reader to usage-only before tool/continuation acknowledgement. Ignore output and tool payloads, including validation of discarded output; preserve provider usage through normal observers. Drain only the current response, with a 256 KiB wire budget; host timeout/cancellation closes the reader. Optional tail transport errors do not invalidate already accepted text. No background reader or next request is started.
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
