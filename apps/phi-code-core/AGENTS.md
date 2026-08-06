# AGENTS — phi-code-core

## Role

phi-code **product runtime**: `SessionHost` + `TurnDriver` over kernel generation.
Depends on `phi-kernel` + `phi-ext-llm`. UI is `phi-code-ui`. Process env is host-owned (CLI).

## Boundaries

- **Durable SoT:** kernel `Transcript` (via `SessionHost`)
- **Generation:** kernel `SendQueue` + `AgentPorts` (no custom mpsc/event enums)
- **Observe:** `KernelEvent` bus → product view
- **Provider HTTP/SSE:** `phi-ext-llm` only (composed by `TurnDriver`)
- **History policy:** product default (`ChatTextOnly`) lives here, not in phi-ext-llm
- **Env keys:** product-owned names (`PHI_*`) but **loaded by host (CLI)**, not this crate
- **UI:** `phi-code-ui` only

## Public surface

- `SessionHost` — `submit_user` / `poll_events` → `PollBatch` / `history` → `Result` / `is_busy`
- `PollBatch` — `events`, `lagged` (must resync history), `pump_error` (host-private, not a bus forge)
- `TurnDriver` — product session driver: `from_config` / `unconfigured`, submit/tick, usage counts
- `ContextWindowSize` — token window size (≥1); host-supplied, not read from env here
- DTOs: `SubmitOutcome`, `TickResult`, `ChannelInfo` (`ModelId` / `Option<ApiStyle>` / `ApiBase` / `ContextWindowSize`), `UsageInfo`
- Re-exports: `LlmConfig`, `ModelId`, `ApiBase`, `ApiKey`, `ApiStyle`, `KernelEvent`, `TurnItem`, `SessionId`, …

## Pump rules

- Single worker via `pump_running` CAS + exit re-check (no stuck pending jobs)
- Never forge `KernelEvent` / synthetic `JobId` on pump failure

## Forbidden

- Process env reads (`std::env`, `PHI_*` loading) — hosts supply `LlmConfig`
- UI / ratatui / scrollback
- Parallel progress enums (`TurnProgress`)
- Custom `agent.run` spawn loops that bypass `SendQueue`
- Owning provider wire parsers
- Writing errors into fake assistant transcript rows
- `da-*` imports
