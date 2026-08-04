# phi-ext-subagent

Parent/child agent **delegation protocol** for [phi](../../README.md): tool-shaped
subagent spawn. v0 ships the **interface surface** only — the types and ports are
final; the runner lands in v1.

## In scope

| Area | API |
|------|-----|
| Delegation | `SubagentRequest`, `SubagentResult`, `Subagent` |
| Binding | `SubagentSource`, `NoSubagents`, `MapSubagents` |
| Spawn | `SpawnMode` (`Ephemeral` / `TreeChild`), `SessionSpawner`, `SpawnBudget` |

**Tool-shaped delegation:** from the parent turn's view one subagent invocation is
exactly one tool call — `SubagentRequest` carries the kernel `ToolCallId` +
`ToolName` plus an opaque `input` — and closes with one
`SubagentResult { status, output }`. `status` is the **sole outcome authority**
(kernel `ToolResultStatus`); `output` is opaque and never sniffed. No stream, no
partials.

**v0 boundary:** no runner, no execution. `Subagent` is implemented by
products/adapters; `SessionSpawner` (the `TreeChild` derive port) is wired by
`phi-ext-tree-agent` or a product; budget enforcement is the v1 runner's job.

## Boundary: protocol vs product

| Concern | Owner |
|---------|--------|
| Tool-result outcome | Kernel `ToolResultStatus` (reused; no re-encoding) |
| `input` / `output` | Opaque `serde_json::Value` — never interpreted |
| Subagent binding | `SubagentSource` (**binding**, not ACL — permission is product-side) |
| `TreeChild` derive | `SessionSpawner` — v1: `phi-ext-tree-agent` / product |
| Spawn budget enforcement | v1 runner (this crate declares `SpawnBudget` only) |

## Out of scope

- Runner / executor → v1 (this crate keeps the interface face only)
- ACL / permission engines (product)
- Session graph derive / close → `phi-ext-tree-agent`
- Stream / partial subagent output (tool-shaped request/result only)
- **Mailbox** naming (reserved for a future agent async-notification bus)

## Develop

```bash
cd open-source
cargo test -p phi-ext-subagent
```
