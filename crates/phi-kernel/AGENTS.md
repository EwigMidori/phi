# AGENTS — phi-kernel

## Role

Minimal agent **mechanisms** for **phi**. No tree/graph, no Daan product strategy, no provider loop (HTTP/SSE adapters live in `phi-ext-llm`).

## Public surface

- Ids / `KernelError` / `KernelEvent`
- `AgentRuntime` / `TurnRequest` / `AgentEvent` / `Usage` (provider metering observation)
- `OneshotModel` / `OneshotRequest`: one standalone multimodal completion with explicit instructions and cancellation, no agent/tool loop. Reuses `AgentRun` for owned response events and cleanup; session identity resolves image assets only.
- `MessageContent` / `ContentPart` / `ImageId` — ordered user input; no bytes IO in kernel
- `TailState` / `TurnRequest.tail_state` — independent transient state; adapter calls `materialize_history` once after projection; product owns policy, kernel owns placement
- `TurnCancel::cancelled` — wakeable preparation/stream cancellation; token belongs to a queue claim
- `TurnCancel::child` inherits ancestor cancellation without cancelling ancestors or siblings; direct waiter composition, no background relay tasks.
- `KernelEvent::GenerationUsage` — notice only; **not** transcript
- `AgentPorts` = runtime + `TurnMaterials` (pump inject); `SourcesTurnMaterials` = default prepare from sources
- `AgentPrefix` / `AgentPrefixSource` / `PreambleSection` / `SkillSlug` / `SkillDesc` / `ToolSpec` (`name: ToolName`)
- `ToolCallSealPolicy` / `ToolCallSealSource` (seal; **opt-in** — default posture `LeaveOpen`; no enum default; usually behind materials)
- **`SendQueue`** / `GenerationJob` / `SessionDirectory` (never call this "mailbox")
- `enqueue_many` validates and appends an ordered batch atomically; pending/running or batch-duplicate JobIds are rejected without partial enqueue.
- `Transcript` / `truncate_from` / `record_reasoning` (trait in `transcript/`); reference impl `InMemoryTranscript` (re-exported)
- Reasoning is a durable sibling row (flushed from the generation turn before text/tools/end); live path is still `GenerationReasoningDelta` notices

## Prefix vs turn (ownership)

| Layer | Fields | Owner |
|-------|--------|--------|
| **Prefix** | `preamble` (`Vec<PreambleSection>`), `tools`, `skill_index` (`BTreeMap<SkillSlug, SkillDesc>`) | Session/agent **binding** via `AgentPrefixSource`; `TurnRequest.prefix` is a **read-only snapshot** |
| **Turn** | `history` (`Vec<TurnItem>`), independent `tail_state`, `cancel`, `job_id`, `tool_call_seal` (snapshot) | This generation; assembled by `TurnMaterials::prepare` |
| **Transcript read** | `load_rows` → `TranscriptRow { id, item }`; `load_turn_history` = items only | Durable authority vs model material |
| **Oneshot** | `OneshotText::complete(&str) -> String` | Bare text; no product prefix/tools; not session labeling policy |
| **Pump inject** | `AgentPorts` { agent, materials } | Directory/queue plumbing — not a domain aggregate |

- `AgentPrefix::render_preamble()` → `<section-name>content</section-name>` per section, joined by `\n`
- Skill **index** only in `skill_index` (short desc); **bodies** must not live in prefix
- Changing prefix across turns can bust provider prefix cache and dominate cost at large contexts — treat as explicit binding updates, not casual per-turn edits
- Kernel does **not** hard-freeze prefix bytes; it does **not** put prefix authority on Turn

## Generation turn (send_queue)

- `AgentRuntime::run` returns owned `AgentRun`; the consumer uses `next` and always awaits `close_and_join`, including commit failures.
- `AgentRun::map_stream` transforms the pull stream while retaining its lifecycle owner; early transformed termination still requires `close_and_join` for upstream cleanup.
- `ResponseUsageDrain` is an optional explicit run capability: `begin` prevents further tool execution/provider requests and permits only usage/terminal events from the current response. Stream transforms/observers retain this capability; hosts bound the wait and still close/join the run. Kernel does not choose product delimiters or wait durations.
- `GenerationTurn` buffers only the current visible response. `ModelResponseCompleted` commits the complete ordered response batch; terminal never writes a merged assistant body again.
- `EffectApplier` is the sole commit/publish path. `Transcript::commit_generation` success means durable host commit. A response is committed before the adapter is polled again to execute tools; a result is committed before the next tool/provider request.
- `GenerationCommit`: `Enqueue`, `Start`, `Response`, `ToolResult`, `Finish`; `TranscriptSession` persists rows plus generation records.
- `TranscriptRow.generation` preserves `JobId`, user `MessageId` anchor, `ModelResponseId`, and response completeness. `ordered_rows` and `history_for` use causal input order, not physical append order.
- `ToolArguments` retains exact provider JSON text, including malformed arguments; parsing belongs at the execution boundary.
- `ProviderContinuation` is opaque scoped replay material. A materialized `TurnItem::ModelResponse` groups durable flat rows for provider encoding; nested materialized groups cannot be persisted.
- `GenerationModelResponseCommitted` and tool notices are projected only after commit. `GenerationStart` carries the committed version.
- Tool ledger keys are `(ModelResponseId, ToolCallId)`; duplicate/open-orphan results are errors. Sealing remains opt-in (`LeaveOpen` by default); products may select `SealAlways`.
- Stop captures and cancels the same queue claim atomically, awaits run cleanup, commits partial text and incomplete results, then commits terminal and releases the seat. `pause_claim`/`resume_claim` support host mutations; `stop_and_wait` waits only for the captured job.
- Failed commits stop later claims, preserve the actual fault JobId, and emit an error observation at the last committed version. No provider/tool replay on a failed commit.

## Allowed (tools / policy shapes)

- `AgentEvent` response batches and tool results (`ToolApprovalRequired` remains an observation shape, never an approval engine)
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
