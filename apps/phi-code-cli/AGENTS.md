# AGENTS — phi-code-cli

## Role

Thin **terminal host** over `phi-code-core` + `phi-code-ui`.

| Module | Object |
|--------|--------|
| `main.rs` | Terminal lifecycle only (`run_terminal_host`) |
| `shell.rs` | `AgentShell` — tick / draw / handle routing |
| `scrollback_pane.rs` | History, selection, scrollbar, nav keys |
| `prompt_pane.rs` | TextArea + paste policy |
| `turn_driver.rs` | Env → runtime + product `ChatTextOnly` projector; `SessionHost`; `SubmitOutcome` |

## Forbidden

- Duplicating scrollback / selection / paint / paste logic (belongs in `phi-code-ui`)
- Growing `main.rs` with event tables again
- Editing vendored crates under `vendor/` except deliberate upgrades
