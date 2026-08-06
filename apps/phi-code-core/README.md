# phi-code-core

Product runtime for phi-code (LLM adapter + turn runner).

## LLM config

| Env | Meaning | Default |
|-----|---------|---------|
| `PHI_API_KEY` | Bearer token (**required**) | — |
| `PHI_API_BASE` | API root | `https://api.openai.com/v1` |
| `PHI_MODEL` | Model id | `gpt-4o-mini` |
| `PHI_API_STYLE` | `responses` \| `completions` | `responses` |

| Style | Endpoint |
|-------|----------|
| `responses` | `POST {base}/responses` |
| `completions` | `POST {base}/chat/completions` |

Objects: `OpenAiCompatRuntime`, `SessionTurnRunner`.

```bash
cargo test -p phi-code-core
```
