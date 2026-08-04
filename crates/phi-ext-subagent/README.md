# phi-ext-subagent

Parent/child agent **delegation protocol** for [phi](../../README.md): tool-shaped
subagent spawn. v0 ships the **interface surface** only — the types and ports are
final; the runner lands in v1.

## In scope

| Area | API |
|------|-----|
| Delegation | `SubagentRequest` (`TreeChild` / `Ephemeral`), `SubagentResult`, `Subagent` |
| Dispatch | `DelegationContext`, `SubagentResolver`, `NoSubagents`, `MapSubagents` |
| Spawn | `SessionSpawner` |

**Tool-shaped delegation:** from the parent turn's view one subagent invocation is
exactly one tool call — `SubagentRequest` carries the kernel `ToolCallId` +
`ToolName` plus an opaque `input` — and closes with one
`SubagentResult { status, output }`. `status` is the **sole outcome authority**
(kernel `ToolResultStatus`); `output` is opaque and never sniffed. No stream, no
partials.

**Two messages, one wire:** `SubagentRequest` is an outer enum (`mode` tag):
`TreeChild` derives a **child session node** and carries `parent_session_id` — a
**real parent** (the derivation source); `Ephemeral` has **no session node** and
carries `origin_session_id` — **not a parent**, only binding-material
preparation for the child generation.

**Caller-first dispatch:** the initiator decides whether a call is a delegation
and which subagent executes it when one is known. `SubagentResolver` is
consulted only for a confirmed delegation with no explicitly chosen subagent,
resolving from a `DelegationContext` (a deliberate proper subset of the request
— `mode` is decided by the initiator, never by the resolver), and **always
answers** — an unresolved case is a configuration error, never a normal
outcome. Resolution may key on `tool_name`, `input` content, or other facts —
it is **not** a per-session binding.

**v0 boundary:** no runner, no execution. `Subagent` is implemented by
products/adapters; `SessionSpawner` (the `TreeChild` derive port) is wired by
`phi-ext-tree-agent` or a product; budget enforcement is the v1 runner's job.

## Boundary: protocol vs product

| Concern | Owner |
|---------|--------|
| Tool-result outcome | Kernel `ToolResultStatus` (reused; no re-encoding) |
| `input` / `output` | Opaque `serde_json::Value` — never interpreted |
| Subagent dispatch | `SubagentResolver` (**per-call resolver** via `DelegationContext`, not ACL — permission is product-side) |
| `TreeChild` derive | `SessionSpawner` — v1: `phi-ext-tree-agent` / product |
| Spawn caps | **Not in v0** — v1 runner consults an injected `SpawnPolicy` port (product policy; mechanism prescribes no shape) |

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
