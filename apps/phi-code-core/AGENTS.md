# AGENTS — phi-code-core

## Role

phi-code product runtime: **session host** over kernel generation.
Depends on `phi-kernel` only. LLM wire adapters are **`phi-ext-llm`**.

## Boundaries

- **Durable SoT:** kernel `Transcript` (via `SessionHost`)
- **Generation:** kernel `SendQueue` + `AgentPorts` (no custom mpsc/event enums)
- **Observe:** `KernelEvent` bus → product view
- **Provider HTTP/SSE:** `phi-ext-llm` only
- **UI:** `phi-code-ui` only

## Public surface

- `SessionHost` — `submit_user` / `poll_events` → `PollBatch` / `history` → `Result` / `is_busy`
- `PollBatch` — `events`, `lagged` (must resync history), `pump_error` (host-private, not a bus forge)
- Re-exports: `KernelEvent`, `TurnItem`, `SessionId`, …

## Pump rules

- Single worker via `pump_running` CAS + exit re-check (no stuck pending jobs)
- Never forge `KernelEvent` / synthetic `JobId` on pump failure

## Forbidden

- Parallel progress enums (`TurnProgress`)
- Custom `agent.run` spawn loops that bypass `SendQueue`
- Owning provider wire parsers
- Writing errors into fake assistant transcript rows
- `da-*` imports
