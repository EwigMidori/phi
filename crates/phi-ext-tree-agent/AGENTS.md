# AGENTS — phi-ext-tree-agent

## Role

Session **tree** mechanisms for **phi**. One root, exactly one parent per node,
Live/Tombstoned lifecycle, injected close policy, caller-projected tree events.
Tree only — no graph, no search (v1), no SQLite, no product orchestration.

## Public surface

- `SessionTree` (`open` / `add_root` / `derive` / `close`; accessors `node` / `incoming_edge` / `children` / `path_to_root` / `is_leaf` / `root`; `snapshot` / `persist`)
- `NodeState` (`Live` / `Tombstoned`), `EdgeKind` (`WithHistory` / `WithoutHistory`), `ParentEdge`, `TreeEdge`
- `ClosePolicy` / `NodeCloseContext` / `CloseDisposition` + standard `StandardClosePolicy` (explicit inject at `open`; **no `Default`** on policy types)
- `TreeStore` / `TreeSnapshot` (+`validate`) / `InMemoryTreeStore`
- `TreeEvent` (`nodeCreated` / `edgeCreated` / `nodeTombstoned` / `nodeRemoved`) — caller-projected, no built-in bus
- `TreeError`

## Rules

- **Tree, not graph:** each node has exactly one parent (root has none); no multi-parent / DAG / merge. `derive` creates a new Live child from a Live parent.
- Node identity is kernel `SessionId` — reuse it, never mint a new id type.
- The parent and the edge kind are **one fact**: the aggregate stores `Option<ParentEdge>` on the child (never two independent `Option`s that could drift); query it via `incoming_edge`.
- `Tombstoned` keeps the node in the topology (subtree stays connected); it cannot be derived from or focused.
- Close splits via the injected `ClosePolicy` (`StandardClosePolicy` implements the Daan rule: leaf → hard remove; non-leaf → tombstone; tombstoned leaf → hard remove; tombstoned non-leaf → no-op). The tree resolves leaf-ness and enforces invariants: hard remove of a non-leaf is `CloseRefused` even for custom policies.
- Close policy `disposition` runs **under the tree lock**: implementations must be stateless, cheap, and must not call back into any `SessionTree` method (same-lock reentry would deadlock); every needed fact is in `NodeCloseContext`.
- Mutations return `Vec<TreeEvent>` for the caller to project; nothing is persisted or published implicitly. Persist via `persist()` / `snapshot()`.
- `SessionTree` is `Arc<Mutex<…>>` + `&self`; `Clone` shares one logical tree (pump + handlers in v1). The internal map is a `BTreeMap` keyed by id string (kernel `SessionId` has no `Ord`) so snapshots are deterministic and the save/load round trip is lossless.
- Duplicate `add_root` and duplicate closes are **loud** (error or no-op-without-event), never silent divergent behavior.

## Forbidden

- Graph semantics (multi-parent, DAG, merge, cascade delete)
- v1 search surface (`continue_branch` / `branch` / `backtrack` / `evaluate`, `TreeSearchPolicy`, `TreeEvaluator`, `TreeBudget`, `SessionPort`) — design extension points only
- Built-in `EventBus` in this crate; implicit persistence on mutations
- `Default` on `ClosePolicy` types (explicit inject at `open`)
- `da-*` imports / product logic beyond `StandardClosePolicy`
- Reusing kernel `KernelError` as the crate error type (`TreeError` is this crate's own)
- New id types (reuse kernel `SessionId`)
- Calling the tree a graph / mailbox / store

## Dependencies

Only crates declared in this package's `Cargo.toml`. No `da-*`.
