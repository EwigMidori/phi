# phi-ext-tree-agent

Session **tree** aggregate for **[phi](../../README.md)**: **pure topology +
atomic, structure-preserving operations**. One root, exactly one parent per
node. Lifecycle state (Live/Tombstoned, close semantics) is **not** a tree
fact — products orchestrate it out-of-band with `remove` / `remove_subtree` /
`reparent`.

## In scope

| Area | API |
|------|-----|
| Aggregate | `SessionTree` (`open` / `add_root` / `derive` / `remove` / `remove_subtree` / `reparent`; accessors `contains` / `incoming_edge` / `children` / `path_to_root` / `is_leaf` / `root`; `snapshot` / `persist`) |
| Model | `EdgeKind` (`WithHistory` / `WithoutHistory`), `ParentEdge`, `TreeEdge` |
| Persistence | `TreeStore` (snapshot `save` / `load`), `TreeSnapshot` (+`validate`), `InMemoryTreeStore` |
| Events | `TreeEvent` (`nodeCreated` / `edgeCreated` / `nodeRemoved` / `edgeReparented`) — projected by the caller, no built-in bus |
| Errors | `TreeError` |

### Decisions

| Decision | Choice | Why (one line) |
|----------|--------|-----------------|
| Lifecycle | **Outside the tree** — no `NodeState`, no close policy | Lifecycle is product state; the tree only guarantees structure-preserving atomic operations |
| Node identity | Reuses kernel `SessionId` | Opaque ids, no new id type |
| Internal state | `Arc<Mutex<BTreeMap<String, …>>>` + `&self` | Kernel `SessionDirectory` pattern; v1 pump + handlers share one tree; `BTreeMap` for deterministic snapshots |
| New id on `derive` | Tree generates via `SessionId::generate()` | Callers cannot inject duplicates; id policy lives in one place |
| Duplicate `add_root` | Loud error (`NodeAlreadyExists` / `RootAlreadyExists`) | A tree has exactly one root; a silent no-op would hide double-creation bugs |
| `remove` | Leaf-only; non-leaf → `WouldOrphan` | Removing a non-leaf would orphan its children; the cascade is explicit (`remove_subtree`) |
| `remove_subtree` | Single-lock atomic cascade; one `NodeRemoved` per node, top-down | Projections drop parents before children; one lock keeps the mutation atomic |
| `reparent` | Acyclic by construction: self / descendant target → `WouldCycle`; root → `CannotReparentRoot`; missing operand → `NodeNotFound` | Reparenting into the child's subtree would create a cycle; the root has no parent by definition |
| Edge storage | Parent edge lives on the child record as one `Option<ParentEdge>` | Parent and kind always co-occur; two `Option`s would be a hand-written invariant |
| Incoming edge accessor | `incoming_edge() -> Result<Option<ParentEdge>>` | One message returns the complete fact (parent + kind) |
| Existence check | `contains(&SessionId) -> bool`, never errors | `node()` is gone (it returned lifecycle state); existence is the only state-free query needed |
| TreeNode folding | No public `TreeNode`; internal `NodeRecord` is private | Avoids repeating `session_id` in both the entity and every query parameter (audit-confirmed) |
| Persistence | Explicit `persist()` / `snapshot()`; point-in-time, not a consistency barrier | Mutations never write implicitly; callers coordinate for consistent on-disk state |

## Out of scope

- Lifecycle state (Live / Tombstoned) and close policies → product-side orchestration over the atomic operations
- Graphs: multi-parent / DAG / merge
- Implicit cascade delete (explicit `remove_subtree` only)
- Search (v1): `continue_branch` / `branch` / `backtrack` / `evaluate` + `TreeSearchPolicy` / `TreeEvaluator` / `TreeBudget` / `SessionPort` — extension points left open (shared `&self` aggregate)
- SQLite / product store backend
- Built-in event bus (the caller projects `Vec<TreeEvent>`)
- `da-*` product logic

## Develop

```bash
cd open-source
cargo test -p phi-ext-tree-agent
```
