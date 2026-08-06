# phi-code-ui

Terminal UI for phi-code. Owns scrollback view, character selection, painting,
paste policy, and clipboard — not product/runtime orchestration.

## Objects (Kay)

| Object | Responsibility |
|--------|----------------|
| `Scrollback` | Conversation view over kernel `TurnItem` (layout, fold, stream, viewport) |
| `EntryView` | How one turn presents itself (accent, height, lines) — private collaborator |
| `ProductMarkdown` | Pretty markdown rendering shared by height + paint + copy plain text |
| `ScrollbackPainter` | Paints segments + feeds streaming markdown + builds selection geometry |
| `HistoryScrollbar` | Grok-style gap+track scrollbar (follow dim, click/drag jump) |
| `Selection` | Character-level selection (hit-test, drag, highlight, reconstruct copy) |
| `PastePolicy` | Bracketed / Ctrl+V paste → inline or chip |
| `SystemClipboard` | Host clipboard behind `ClipboardProvider` |
| `HorizontalLayout` | Accent \| pad \| content \| pad columns |

```bash
cargo test -p phi-code-ui
```
