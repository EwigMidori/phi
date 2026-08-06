# AGENTS — phi-code-cli

## Role

Thin **terminal host** over `phi-code-core` + `phi-code-ui`.

## Layers (MVVM-ish — not a framework)

| Layer | Path | Owns |
|-------|------|------|
| Host | `main.rs` | Terminal lifecycle, poll loop only |
| Application / Model | `turn_driver.rs` | Env → LLM → `SessionHost`; submit/tick; usage **counts**; product history policy. **No UI imports.** |
| View + chrome | `shell/` | Layout, paint, focus, panes, status **formatting**, kernel-event → scrollback **projection** |

`AgentShell` may **compose** `TurnDriver` but must not own product policy.  
`TurnDriver` must not import `phi_code_ui` / ratatui or mutate scrollback.

## Free functions

**Free functions with read/write side effects are forbidden** (mutate objects, env, I/O, global state).  
Side effects belong on the object that owns the state (methods).  
Free functions may only be **pure**: arguments → return value (e.g. `status_format`, `rect_contains`, `classify_note`).

## Module map

| Module | Object |
|--------|--------|
| `main.rs` | `run_terminal_host` |
| `turn_driver.rs` | `TurnDriver`, `SubmitOutcome`, `TickResult`, `ChannelInfo`, `UsageInfo`, env/config, `ChatTextOnly` |
| `shell/mod.rs` | `AgentShell` — route + wire model↔view |
| `shell/scrollback_pane.rs` | History, selection, scrollbar, nav keys, **event→view projection** |
| `shell/status_format.rs` | Pure formatters: channel/usage → status strings |
| `shell/quit_protocol.rs` | Double-Ctrl+C arm/confirm quit |
| `shell/status_bar.rs` | `StatusBar` — summary + expandable panel |
| `shell/prompt_pane.rs` | TextArea + paste policy |

## Forbidden

- Duplicating scrollback / selection / paint / paste logic (belongs in `phi-code-ui`)
- Growing `main.rs` with event tables again
- **UI under `turn_driver`** — no `Scrollback`, no status string formatting, no ratatui
- **Product policy under `shell/`** — no LLM config, no `SessionHost` construction, no history projector, no usage aggregation
- **Side-effecting free functions** — projection/env/I/O must be methods on owners
- **Adding paint/product logic onto `AgentShell`** — use panes / format / `TurnDriver`
- Re-homing `turn_driver` into `shell/` “for convenience”
- Editing vendored crates under `vendor/` except deliberate upgrades
