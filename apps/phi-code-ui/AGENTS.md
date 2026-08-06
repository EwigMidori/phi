# AGENTS — phi-code-ui

## Role

TUI layer for phi-code. Depends on `phi-kernel` for conversation SoT (`TurnItem`).
Does **not** own agent runtime / product orchestration (`phi-code-core`).

## Style: Kay OOP

- Prefer **objects with private state** that answer **messages** (methods).
- Avoid free-function pipelines over naked data (`fn paint_x(...)`, `fn accent_for(...)`).
- Collaborators hide implementation; callers only send messages.
- Vendor crates under `phi-code-cli/vendor` stay as-is (no rewrites).

## Forbidden

- Parallel chat-message enums that duplicate `TurnItem`
- Putting agent/runtime policy here
- `da-*` imports
