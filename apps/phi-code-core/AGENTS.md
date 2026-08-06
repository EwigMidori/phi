# AGENTS — phi-code-core

## Role

phi-code product runtime: **turn orchestration** only. Depends on `phi-kernel`.
LLM wire adapters are **`phi-ext-llm`** (framework ext), not this crate.

## Boundaries

- **SoT for conversation content:** kernel `TurnItem`
- **Agent contract:** `AgentRuntime` / `AgentEvent` only (no ad-hoc text stream API)
- **Provider HTTP/SSE:** `phi-ext-llm` only
- **UI:** `phi-code-ui` only

## Step-1 product pieces

- `SessionTurnRunner` — non-blocking poll of kernel `AgentEvent` for CLI tick
- Composition root (CLI) injects `Arc<dyn AgentRuntime>` from `phi-ext-llm`

## Public surface

Crate root only. Implementation modules (`turn_runner`) are private.
Kernel re-exports only types on this crate’s API (`AgentEvent`, `AgentRuntime`,
`SessionId`, `TurnItem`).

## Forbidden

- Parallel “chat message” types that duplicate `TurnItem`
- Parallel progress/event enums that duplicate `AgentEvent` (e.g. `TurnProgress`)
- Owning provider wire parsers / OpenAI-compat SSE
- Owning ratatui / scrollback modules
- `da-*` imports
