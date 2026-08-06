# AGENTS — phi-code-cli

## Role

Thin binary over `phi-code-core` + `phi-code-ui`. Keep small; coordinate
objects by message, do not re-implement UI policy here.

## Forbidden

- Duplicating scrollback / selection / paint / paste logic (belongs in `phi-code-ui`)
- Editing vendored crates under `vendor/` except deliberate upgrades
