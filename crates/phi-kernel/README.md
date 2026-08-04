# phi-kernel

First package of **[phi](../../README.md)**: minimal agent **mechanisms**.

## In scope

| Area | API |
|------|-----|
| Ids | `SessionId`, `JobId`, `MessageId` |
| Errors | `KernelError` |
| Agent contract | `AgentRuntime`, `TurnRequest`, `AgentEvent`, `AgentPrefix`, `TurnMaterials`, `AgentPorts` |
| Generation | **`SendQueue`**, `GenerationJob`, `SessionDirectory` (`AgentPorts` = runtime + materials) |
| History | `Transcript` (`ensure_live`, record, `truncate_from`, `load_turn_history`), `InMemoryTranscript` |
| Events | `KernelEvent` (generation-class only), `EventBus` |

**Stream commit:** `EffectBatch` writes are recorded then **projected** to tool bus events; pure `notices` follow. Terminal jobs use `TurnOutcome` (`apply_terminal`), not `EffectBatch`. Tool success path is write-only in the turn (no dual-built Record+Emit).

**Turn history:** `TurnRequest.history` is the full interleaved sequence (user / assistant / tool rows in transcript order) — one ordered `Vec<TurnItem>`, no separate dialogue / tool projections to reassemble. Tool `input` / `output` stay opaque to the kernel.

## Boundary: tools / approval / prefix (kernel vs adapter)

| Concern | Owner |
|---------|--------|
| Permission / approval policy | **Not** kernel — product / `AgentRuntime` **adapter** |
| Tool execution | Adapter / product |
| `AgentEvent::ToolApprovalRequired` | Stream **observation shape only** (not a policy engine) |
| `TurnMaterials` / `SourcesTurnMaterials` | `prepare` → `TurnRequest` (prefix + seal snapshots); sources stay ISP-split underneath |
| `AgentPorts` | Plumbing bag: `agent` + `materials` (not a domain aggregate) |
| `ToolCallSealPolicy` / `ToolCallSealSource` | Incomplete tool **ledger seal** — **opt-in** (default posture: `LeaveOpen`; via materials; no enum default; not ACL) |
| `AgentPrefix` (`preamble` sections, `tools`, `skill_index`) | **Binding** via `AgentPrefixSource`; Turn carries a snapshot only |
| Preamble render | `AgentPrefix::render_preamble()` → `<name>content</name>` per section |
| Skill index | `BTreeMap<SkillSlug, SkillDesc>` — catalog only; **no** skill bodies |
| Prefix change cost | Changing prefix across turns can bust cache and dominate cost at large contexts — explicit binding updates |
| Tool-result outcome | Explicit `ToolResultStatus` on event / record / observation — `output` is **opaque** (never sniffed) |
| `ToolCallId` / `ToolName` | Provider correlation key + tool catalog name (not document PKs); used on events, ledger, transcript |

## Out of scope

- Session **tree** / edges / fork / tombstone close → `phi-ext-*`
- Handout derive
- Product prefs, `daan` paths, HTTP, Rig
- Approval / ACL engines (product adapter)
- **Mailbox** naming (reserved for a future agent async-notification bus)

## Develop

```bash
cd open-source
cargo test -p phi-kernel
```
