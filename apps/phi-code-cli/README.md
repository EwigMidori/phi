# phi-code-cli

CLI entry (`phi-code`). Grok-like stack + prompt basics.

| Key | Action |
|-----|--------|
| ←→ Home End | move cursor |
| Backspace Delete | delete |
| Enter | send line to scrollback |
| Shift/Alt+Enter, trailing `\`+Enter | newline |
| paste | bracketed paste |
| Ctrl+M | toggle multiline |
| Tab | focus prompt ↔ scrollback |
| click prompt / scrollback | focus that pane; click in prompt sets cursor |
| wheel on scrollback | scroll history |
| Esc | quit |

```bash
cargo run -p phi-code-cli
```
