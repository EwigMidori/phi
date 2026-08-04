# phi-ext-tree-agent

Session **tree** aggregate for **[phi](../../README.md)**: one root, exactly one
parent per node, Live/Tombstoned lifecycle, injected close policy, caller-projected
tree events. Tree only — no graph.

## In scope

| Area | API |
|------|-----|
| Aggregate | `SessionTree` (`open` / `add_root` / `derive` / `close`; accessors `node` / `incoming_edge` / `children` / `path_to_root` / `is_leaf` / `root`; `snapshot` / `persist`) |
| Lifecycle | `NodeState` (`Live` / `Tombstoned`), `EdgeKind` (`WithHistory` / `WithoutHistory`), `ParentEdge`, `TreeEdge` |
| Close rule | `ClosePolicy` / `NodeCloseContext` / `CloseDisposition`; standard `StandardClosePolicy` |
| Persistence | `TreeStore` (snapshot `save` / `load`), `TreeSnapshot` (+`validate`), `InMemoryTreeStore` |
| Events | `TreeEvent` (`nodeCreated` / `edgeCreated` / `nodeTombstoned` / `nodeRemoved`) — projected by the caller, no built-in bus |
| Errors | `TreeError` |

### Decisions

| Decision | Choice | Why (one line) |
|----------|--------|-----------------|
| Node identity | Reuses kernel `SessionId` | Opaque ids, no new id type (`generate` / `as_str` / `FromStr` / serde transparent) |
| Internal state | `Arc<Mutex<BTreeMap<String, …>>>` + `&self` | Kernel `SessionDirectory` pattern; v1 search runs from the pump while handlers read the same tree; `BTreeMap` for deterministic snapshots |
| New id on `derive` | Tree generates via `SessionId::generate()` | Callers cannot inject duplicates; id policy lives in one place |
| Duplicate `add_root` | Loud error (`NodeAlreadyExists` / `RootAlreadyExists`) | A tree has exactly one root; a silent no-op would hide double-creation bugs |
| Leaf detection | Tree computes, passes into `NodeCloseContext` | Policy stays stateless; the tree is the single topology authority |
| Hard remove of non-leaf | `CloseRefused` error | Would orphan children (dangling edges) — the tree enforces invariants against any policy |
| Constructor name | `open(store, policy)` loads + validates | It is open/load semantics, not a purely-fresh `new` |
| Persistence | Explicit `persist()` / `snapshot()` | Mutations never write implicitly (same stance as events) |
| Edge storage | Parent edge lives on the child record as one `Option<ParentEdge>` | Single-parent tree: each edge is owned by its child; parent and kind always co-occur, so two `Option`s would be a hand-written invariant |
| Incoming edge accessor | `incoming_edge() -> Result<Option<ParentEdge>>` | One message returns the complete fact (parent + kind); a bare-parent accessor would drop the kind |
| TreeNode folding | No public `TreeNode` entity; internal `NodeRecord` is private, projection via `node()` + accessors | Avoids repeating `session_id` in both the entity and every query parameter (audit-confirmed) |
| Tombstoned non-leaf close | No-op, no event | Already soft-deleted; a duplicate `NodeTombstoned` would be a ghost event |

## Out of scope

- Graphs: multi-parent / DAG / merge / cascade delete
- Search (v1): `continue_branch` / `branch` / `backtrack` / `evaluate` + `TreeSearchPolicy` / `TreeEvaluator` / `TreeBudget` / `SessionPort` — extension points are left open (shared `&self` aggregate, injected ports)
- SQLite / product store backend
- Built-in event bus (the caller projects `Vec<TreeEvent>`)
- `da-*` product logic beyond the injected `StandardClosePolicy`

## Develop

```bash
cd open-source
cargo test -p phi-ext-tree-agent
```
