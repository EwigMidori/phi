# AGENTS — phi-ext-tree-agent

## Role

Session **tree** mechanisms for **phi**: pure topology + atomic,
structure-preserving operations. One root, exactly one parent per node.
**No lifecycle state, no close policy** — products orchestrate their own close
lifecycle out-of-band with `remove` / `remove_subtree` / `reparent`. No graph,
no search (v1), no SQLite.

## Public surface

- `SessionTree` (`open` / `add_root` / `derive` / `remove` / `remove_subtree` / `reparent`; accessors `contains` / `incoming_edge` / `children` / `path_to_root` / `is_leaf` / `root`; `snapshot` / `persist`)
- `EdgeKind` (`WithHistory` / `WithoutHistory`), `ParentEdge`, `TreeEdge`
- `TreeStore` / `TreeSnapshot` (+`validate`) / `InMemoryTreeStore`
- `TreeEvent` (`nodeCreated` / `edgeCreated` / `nodeRemoved` / `edgeReparented`) — caller-projected, no built-in bus
- `TreeError`

## Rules

- **Tree, not graph:** each node has exactly one parent (root has none); no multi-parent / DAG / merge.
- **No lifecycle state in the tree:** no Live/Tombstoned, no close(). Products keep lifecycle out-of-band and drive it with the atomic operations (`remove` leaf-only, `remove_subtree` explicit cascade, acyclic `reparent`).
- Node identity is kernel `SessionId` — reuse it, never mint a new id type.
- The parent and the edge kind are **one fact**: the aggregate stores `Option<ParentEdge>` on the child (never two independent `Option`s that could drift); query it via `incoming_edge`.
- Every mutation is structure-preserving: `remove` refuses non-leaves (`WouldOrphan`); `remove_subtree` cascades atomically under one lock with events **top-down** (parents before children); `reparent` refuses root targets (`CannotReparentRoot`), self / descendant targets (`WouldCycle`), and missing operands (`NodeNotFound`).
- Mutations return `Vec<TreeEvent>` for the caller to project; nothing is persisted or published implicitly. `persist()` captures a point-in-time snapshot and is **not a consistency barrier** — callers coordinate for consistent on-disk state.
- `SessionTree` is `Arc<Mutex<…>>` + `&self`; `Clone` shares one logical tree (pump + handlers in v1). The internal map is a `BTreeMap` keyed by id string (kernel `SessionId` has no `Ord`) so snapshots are deterministic and the save/load round trip is lossless.
- Duplicate `add_root` is **loud** (error), never silent divergent behavior.

## Forbidden

- Lifecycle state (`NodeState` Live / Tombstoned), close policies, `NodeTombstoned` events — lifecycle lives outside this crate
- Graph semantics (multi-parent, DAG, merge)
- Implicit cascade delete (only explicit `remove_subtree`)
- v1 search surface (`continue_branch` / `branch` / `backtrack` / `evaluate`, `TreeSearchPolicy`, `TreeEvaluator`, `TreeBudget`, `SessionPort`) — design extension points only
- Built-in `EventBus` in this crate; implicit persistence on mutations
- `da-*` imports / product logic
- Reusing kernel `KernelError` as the crate error type (`TreeError` is this crate's own)
- New id types (reuse kernel `SessionId`)
- Calling the tree a graph / mailbox / store

## Dependencies

Only crates declared in this package's `Cargo.toml`. No `da-*`.
