# vendor/ (phi-code-cli only)

Third-party UI crates copied **as-is** from Grok Build codegen
(`crates/codegen/…`). Product code lives outside this tree.

| Crate | Role |
|-------|------|
| `xai-ratatui-textarea` | Multi-line terminal textarea (prompt) |
| `xai-ratatui-inline` | Inline / scrollback viewport helpers |
| `xai-grok-markdown-core` | Headless markdown parse config |
| `xai-grok-markdown` | Terminal streaming markdown renderer |

## Rules

1. **Do not edit** sources here to fix phi bugs — change `phi-code-cli` (or workspace deps) instead.
2. Upgrade by re-copying upstream; keep LICENSE/NOTICE.
3. Listed as workspace members so their `dependency = { workspace = true }` resolves from the **phi root** `Cargo.toml`.

License: Apache-2.0 (per crate).
