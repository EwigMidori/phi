# phi-code-cli

CLI entry (`phi-code`). Prompt uses vendored **`xai-ratatui-textarea`**
(see [`vendor/`](./vendor/)).

| Input | Action |
|-------|--------|
| typing / arrows / undo | TextArea (`xai-ratatui-textarea`) |
| Enter | send line → scrollback |
| Shift/Alt+Enter | newline in prompt |
| paste | bracketed paste → `insert_str` |
| Ctrl+M | prefer taller prompt |
| Tab | focus prompt ↔ scrollback |
| click prompt | focus + TextArea mouse |
| wheel on scrollback | scroll history |
| Esc | quit |

```bash
cargo run -p phi-code-cli
```
