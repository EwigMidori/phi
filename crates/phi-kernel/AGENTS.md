# AGENTS — phi-kernel

## Role

Minimal agent **mechanisms** for **phi**. No tree/graph, no Daan product strategy, no provider loop (HTTP/SSE adapters live in `phi-ext-llm`).

## Public surface

- Ids / `KernelError` / `KernelEvent`
- `AgentRuntime` / `TurnRequest` / `AgentEvent` / `Usage` (provider metering observation)
- `KernelEvent::GenerationUsage` — notice only; **not** transcript
- `AgentPorts` = runtime + `TurnMaterials` (pump inject); `SourcesTurnMaterials` = default prepare from sources
- `AgentPrefix` / `AgentPrefixSource` / `PreambleSection` / `SkillSlug` / `SkillDesc` / `ToolSpec` (`name: ToolName`)
- `ToolCallSealPolicy` / `ToolCallSealSource` (seal; **opt-in** — default posture `LeaveOpen`; no enum default; usually behind materials)
- **`SendQueue`** / `GenerationJob` / `SessionDirectory` (never call this "mailbox")
- `Transcript` / `truncate_from` / `record_reasoning` (trait in `transcript/`); reference impl `InMemoryTranscript` (re-exported)
- Reasoning is a durable sibling row (flushed from the generation turn before text/tools/end); live path is still `GenerationReasoningDelta` notices

## Prefix vs turn (ownership)

| Layer | Fields | Owner |
|-------|--------|--------|
| **Prefix** | `preamble` (`Vec<PreambleSection>`), `tools`, `skill_index` (`BTreeMap<SkillSlug, SkillDesc>`) | Session/agent **binding** via `AgentPrefixSource`; `TurnRequest.prefix` is a **read-only snapshot** |
| **Turn** | `history` (`Vec<TurnItem>`), `cancel`, `job_id`, `tool_call_seal` (snapshot) | This generation; assembled by `TurnMaterials::prepare` |
| **Transcript read** | `load_rows` → `TranscriptRow { id, item }`; `load_turn_history` = items only | Durable authority vs model material |
| **Pump inject** | `AgentPorts` { agent, materials } | Directory/queue plumbing — not a domain aggregate |

- `AgentPrefix::render_preamble()` → `<section-name>content</section-name>` per section, joined by `\n`
- Skill **index** only in `skill_index` (short desc); **bodies** must not live in prefix
- Changing prefix across turns can bust provider prefix cache and dominate cost at large contexts — treat as explicit binding updates, not casual per-turn edits
- Kernel does **not** hard-freeze prefix bytes; it does **not** put prefix authority on Turn

## Generation turn (send_queue)

- `GenerationTurn::handle` / `start_effects` / `seal_incomplete_effects` → pure state + `EffectBatch` (`writes` = `TranscriptWrite`, `notices` = pure bus intents; `Disposition`; no transcript/bus in handle)
- **Tool dual-write banned:** durable tool rows are stream SoT; success-path tool call/result are **write only** in handle; bus tool events are **projected after successful record** in the applier. Unknown tool_result (no open ledger id) stays **notice only**.
- **Sole commit path:** `EffectApplier` (`apply_stream(job_id, batch)` for stream/start/seal; `apply_terminal` for Done/Stopped/Error; optional `commit(CommitUnit)`)
- **`apply_stream` hard order:** for each write: `record_*` then immediately projected tool notice; then all `batch.notices`
- `GenerationTurn::drive` owns the agent stream loop; `SendQueue::run_until_idle` is claim → begin → applier → `agent.run` → drive → mark_finished + apply_terminal
- Open tools: `ToolLedger` (open/close; seal opt-in via `ToolCallSealPolicy`, default posture `LeaveOpen`); seal policy comes from `TurnRequest` after `materials.prepare` (typically `SourcesTurnMaterials` → `ToolCallSealSource`)

## Allowed (tools / policy shapes)

- `AgentEvent` tool shapes (`ToolCall`, `ToolResult`, `ToolApprovalRequired` as stream observation only)
- `ToolCallId` (provider correlation key, not document PK) / `ToolName` (catalog name); ledger is `HashMap<ToolCallId, ToolName>`
- `ToolCallSealPolicy` incomplete-tool ledger seal (**opt-in**; not ACL)
- Adapter pass-through of `AgentPrefix` (may be empty tools/skills)
- Tool-result **outcome** is `ToolResultStatus` on `AgentEvent::ToolResult` / `KernelEvent::GenerationToolResult` / `Transcript::record_tool_result` only
- `output` is opaque JSON for model/UI; kernel never interprets its keys

## Forbidden

- Session graph, edges, fork, soft-delete / tombstone policies
- Handout types
- `da-*` imports
- Rig / HTTP / product prefs
- Implementing approval / ACL engines inside phi-kernel
- Holding any approval/ACL port on `SessionDirectory` (do not reintroduce a kernel-side ToolApprove-style trait)
- Product permission engines (Claude-style rules, path ACL, permission nodes, classifiers) — adapter only
- Naming generation control **mailbox** (reserved for future agent notify)
- Deriving status from output JSON (`denied` / `incomplete` bools or `status` string); marker types `DeniedToolOutput` / `IncompleteToolOutput`
- Putting skill **bodies** into `AgentPrefix` / `skill_index`
- Calling binding prefix material “TurnPolicy” or implying Turn owns preamble/tools/skill catalog

## Dependencies

Only crates declared in this package’s `Cargo.toml`. No `da-*`.
