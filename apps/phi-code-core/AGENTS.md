# AGENTS — phi-code-core

## Role

phi-code product runtime: LLM adapter + turn runner. Depends on `phi-kernel`.

## Boundaries

- **SoT for conversation content:** kernel `TurnItem`
- **Agent contract:** `AgentRuntime` / `AgentEvent` only (no ad-hoc text stream API)
- **UI:** `phi-code-ui` only

## Step-1 LLM

- `OpenAiCompatRuntime` — OpenAI-compatible SSE
- `SessionTurnRunner` — non-blocking poll of kernel `AgentEvent` for CLI tick
- Env: `PHI_API_KEY`, `PHI_API_BASE`, `PHI_MODEL`, `PHI_API_STYLE`

## Public surface

Crate root only. Implementation modules (`llm`, `turn_runner`) are private.
Kernel re-exports only types that appear on this crate’s API (`AgentEvent`,
`AgentRuntime`, `SessionId`, `TurnItem`).

## Forbidden

- Parallel “chat message” types that duplicate `TurnItem`
- Parallel progress/event enums that duplicate `AgentEvent` (e.g. `TurnProgress`)
- Owning ratatui / scrollback modules
- `da-*` imports
