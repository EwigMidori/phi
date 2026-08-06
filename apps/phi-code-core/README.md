# phi-code-core

Product runtime: `SessionHost` + `TurnDriver` over kernel `SendQueue` / `Transcript` / `EventBus`.

Depends on [`phi-kernel`](../../crates/phi-kernel) and [`phi-ext-llm`](../../crates/phi-ext-llm).
Terminal UI: [`phi-code-ui`](../phi-code-ui). Process env (`PHI_*`) is loaded by the host (CLI), not this crate.

```bash
cargo test -p phi-code-core
```
