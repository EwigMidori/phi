# AGENTS — phi-code-core

## Role

phi-code product runtime. Depends on `phi-kernel`, `phi-ext-tree-agent`,
`phi-ext-subagent`.

## Boundaries

- **SoT for conversation content:** kernel `TurnItem`
- **UI (scrollback / paint / selection / paste):** `phi-code-ui` only
- Do not reintroduce TUI layout or markdown view code here

## Forbidden

- Parallel “chat message” types that duplicate `TurnItem`
- Owning ratatui / scrollback / selection modules
- `da-*` imports
