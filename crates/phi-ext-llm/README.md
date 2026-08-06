# phi-ext-llm

LLM provider adapters for [phi](../../README.md): OpenAI-compatible HTTP/SSE →
kernel `AgentRuntime` / `AgentEvent`.

## In scope

| Area | API |
|------|-----|
| Config | `LlmConfig`, `ApiStyle`, `LlmConfigError` |
| Runtime | `OpenAiCompatRuntime` |

| `ApiStyle` | Endpoint |
|------------|----------|
| `responses` | `POST {base}/responses` |
| `completions` | `POST {base}/chat/completions` |

## Env helper (`LlmConfig::from_env`)

| Env | Meaning | Default |
|-----|---------|---------|
| `PHI_API_KEY` | Bearer token (**required**) | — |
| `PHI_API_BASE` | API root | `https://api.openai.com/v1` |
| `PHI_MODEL` | Model id | `gpt-4o-mini` |
| `PHI_API_STYLE` | `responses` \| `completions` | `responses` |

Products may construct `LlmConfig` without env.

## Out of scope

Turn runners, TUI, tool hosts, permission engines, kernel changes.

```bash
cargo test -p phi-ext-llm
```
