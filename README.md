# phi

Open-source **agent kernel** (+ future extensions). Daan (`crates/da-*`, `web/`) is a **policy + projection** consumer, not this project’s OSS identity.

| | |
|--|--|
| **Project** | `phi` |
| **Kernel** | [`phi-kernel`](./crates/phi-kernel/) |
| **Extensions** | `phi-ext-tree-agent` · `phi-ext-subagent` · **`phi-ext-llm`** (provider wire → `AgentRuntime`) |
| **Product** | `apps/phi-code-core` (turn runner) + `apps/phi-code-ui` (TUI) + `apps/phi-code-cli` (`phi-code` binary) |
| **Status** | Dense vertical slice in progress |

## Layers

| Layer | Owns |
|-------|------|
| **`phi-kernel`** | Agent contract, **SendQueue**, transcript port, generation events (**no** provider HTTP) |
| **`phi-ext-*`** | Optional mechanisms (tree, subagent, **LLM wire adapters**) |
| **Product (phi-code / Daan)** | Composition, UI, product policy — not provider dialects |

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
    phi-ext-llm/            # OpenAI-compat SSE → AgentRuntime
  apps/
    phi-code-core/          # product turn orchestration
    phi-code-ui/            # TUI objects (scrollback, selection, paint, paste)
    phi-code-cli/           # bin: phi-code (thin coordinator + inject runtime)
      vendor/               # Grok UI crates as-is (textarea, inline, markdown)
```

```bash
cargo test --workspace
cargo run -p phi-code-cli
```

## Relationship to Daan monorepo

- Standalone Cargo workspace (not a member of the root `Cargo.toml`).
- No dependency on `da-*`. Daan may later adapt to these ports.
