# phi

Open-source **agent kernel** (+ future extensions). Daan (`crates/da-*`, `web/`) is a **policy + projection** consumer, not this project’s OSS identity.

| | |
|--|--|
| **Project** | `phi` |
| **Kernel** | [`phi-kernel`](./crates/phi-kernel/) |
| **Tree agent** | `phi-ext-tree-agent` (v0) + `phi-ext-subagent` (delegation interface) |
| **Status** | Stage-1 kernel port in progress |

## Layers

| Layer | Owns |
|-------|------|
| **`phi-kernel`** | Agent contract, **SendQueue**, transcript port, generation events |
| **`phi-ext-*`** | Optional mechanisms (session tree / subagent delegation; tree-agent first) |
| **Product (Daan)** | Close/fork defaults, HTTP, UI rendering, brand paths |

## Naming

Use **SendQueue** for per-session generation control. Do **not** call it mailbox (that name is reserved for a future agent async-notification mechanism).

## Layout

```text
phi/
  Cargo.toml
  crates/
    phi-kernel/
    phi-ext-tree-agent/
    phi-ext-subagent/
```

```bash
cargo test --workspace
```

## Relationship to Daan monorepo

- Standalone Cargo workspace (not a member of the root `Cargo.toml`).
- No dependency on `da-*`. Daan may later adapt to these ports.
