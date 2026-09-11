# phi-kernel

First package of **[phi](../../README.md)**: minimal agent **mechanisms**.

## In scope

| Area | API |
|------|-----|
| Ids | `SessionId`, `JobId`, `MessageId`, `ImageId` |
| Errors | `KernelError` |
| Agent contract | `AgentRuntime`, `TurnRequest`, `AgentEvent`, `AgentPrefix`, `TurnMaterials`, `AgentPorts` |
| Oneshot text | `OneshotText` (`complete(&str) -> String`) — bare complete, no product prefix/tools |
| Generation | **`SendQueue`**, `GenerationJob`, `SessionDirectory` (`AgentPorts` = runtime + materials) |
| History | `Transcript` (`ensure_live`, record, `truncate_from`, `load_turn_history`), `InMemoryTranscript` |
| User content | `MessageContent` with ordered `ContentPart::Text` / `ContentPart::Image` |
| Transient tail | `TurnRequest.tail_state`, `TailState`, `TurnRequest::materialize_history` |
| Events | `KernelEvent` (generation-class only), `EventBus` |

**Stream commit:** `EffectBatch` writes are recorded then **projected** to tool bus events; pure `notices` follow. Terminal jobs use `TurnOutcome` (`apply_terminal`), not `EffectBatch`. Tool success path is write-only in the turn (no dual-built Record+Emit).

**Turn history:** `TurnRequest.history` is the full interleaved sequence (user / assistant / tool rows in transcript order) — one ordered `Vec<TurnItem>`, no separate dialogue / tool projections to reassemble. Tool `input` / `output` stay opaque to the kernel.

**Transcript rows:** durable read is [`Transcript::load_rows`] → `TranscriptRow { id, item }` (stable [`MessageId`] + [`TurnItem`]). [`Transcript::load_turn_history`] is the model-facing strip (items only).

**Image input:** `TurnItem::User.content` and `Transcript::record_user` use the same
`MessageContent`. `ImageId` references immutable host-owned bytes; the kernel neither
opens files nor uploads them. Assistant and reasoning rows remain text. Text-only
hosts construct `MessageContent::text` and use `plain_text()` for display/search.

**Tail state:** per-turn state stays in `TurnRequest.tail_state`, separate from
`history` and binding `prefix`. Products supply policy content through `TailState`.
After history projection, an adapter calls `request.materialize_history(projected)`
once to append the tail after the final user's ordered content. Materialization
returns a new history and does not modify the request or transcript. An empty tail
is a no-op; a nonempty tail without a user message is an error. The tail placement
mechanism belongs to phi; persona policy does not.

**Cancellation:** `TurnCancel::cancelled().await` is wakeable. A queue claim owns
its token, and stopping wakes both adapter preparation (`AgentRuntime::run`,
including image upload) and waiting for the next stream event. Cancellation is
`Aborted`/`GenerationStopped`; pending jobs retain fresh tokens. The pump seat is
released only by its owning worker. `EffectApplier` remains the sole commit path.

**Usage:** `cached_tokens` and `cache_miss_tokens` are optional provider-reported
counters, alongside prompt/completion/total usage. They remain observations; the
kernel does not estimate missing counters or persist them as conversation rows.

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
| `ToolCallId` / `ToolName` | Provider correlation key + tool catalog name (`ToolSpec.name` is `ToolName`); used on events, ledger, transcript |

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
