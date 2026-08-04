# AGENTS — phi-ext-subagent

## Role

Parent/child agent **delegation protocol** surface for **phi**. Tool-shaped spawn:
one delegation is one tool call from the parent turn's view, closed by one
`SubagentResult`. v0 ships the **interface face only** — no runner, no execution.

## Public surface

- `SubagentRequest` (`TreeChild` / `Ephemeral`) / `SubagentResult` / `Subagent`
- `DelegationContext` / `SubagentResolver` / `NoSubagents` / `MapSubagents` (per-call resolver, not a per-session binding)
- `SessionSpawner` / `SpawnBudget`

## Rules

- Reuse kernel types: `ToolCallId`, `ToolName`, `ToolResultStatus`, `SessionId` — no new id types, no status re-encoding
- `status` is the **sole outcome authority**; `input` / `output` stay opaque `serde_json::Value` (never sniff keys)
- `SubagentRequest` and `SpawnBudget` have **no `Default`** — every delegation states its disposition as the variant
- `TreeChild` carries `parent_session_id` (a real parent / derivation source); `Ephemeral` carries `origin_session_id` (materials-only, **never** a parent). The two are distinct messages — a field's meaning never depends on another field.
- `SubagentResolver` is a **per-call resolver** (`DelegationContext`), not a per-session binding; permission lives product-side
- `SessionSpawner` is a declared port; derive implementation belongs to `phi-ext-tree-agent` / product in v1
- Budget enforcement and the runner are **v1**; v0 only declares ports

## Forbidden

- Runner / executor inside v0
- Mailbox naming (reserved for future agent notify)
- ACL / permission engines
- Session graph / derive / close logic (→ `phi-ext-tree-agent`)
- `da-*` imports, product prefs, Rig / HTTP
- Deriving status from `output` JSON

## Dependencies

Only crates declared in this package's `Cargo.toml`. No `da-*`.
