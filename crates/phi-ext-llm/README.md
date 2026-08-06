# phi-ext-llm

OpenAI-compatible HTTP/SSE → kernel `AgentRuntime`.

| API | Role |
|-----|------|
| `LlmConfig` | base / key / model / style (construct explicitly) |
| `ApiStyle` | `responses` \| `completions` |
| `HistoryProjection` | how `TurnItem` history maps to the wire |
| `OpenAiCompatRuntime` | `AgentRuntime` impl |

Products own env vars (e.g. phi-code CLI `PHI_*`).

```bash
cargo test -p phi-ext-llm
```
