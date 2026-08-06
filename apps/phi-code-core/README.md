# phi-code-core

Product runtime for phi-code (LLM adapter + turn runner).

## LLM config (da-server compatible)

| Env | Meaning | Default |
|-----|---------|---------|
| `AGENT_LLM__API_KEY` | Bearer token (**required**) | — |
| `AGENT_LLM__BASE_URL` | API root | `https://api.openai.com/v1` |
| `AGENT_LLM__MODEL` | Model id | `gpt-4o-mini` |
| `AGENT_LLM__API_STYLE` | `responses` \| `completions` | `responses` |

`PHI_API_KEY` / `PHI_API_BASE` / `PHI_MODEL` / `PHI_API_STYLE` are accepted as fallbacks.

| Style | Endpoint |
|-------|----------|
| `responses` | `POST {base}/responses` (DeepSeek-V4-Flash / Codex path) |
| `completions` | `POST {base}/chat/completions` |

Objects: `OpenAiCompatRuntime`, `SessionTurnRunner`.

```bash
cargo test -p phi-code-core
```
