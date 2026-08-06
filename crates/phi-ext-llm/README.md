# phi-ext-llm

OpenAI-compatible HTTP/SSE → kernel `AgentRuntime`.

| API | Role |
|-----|------|
| `LlmConfig` | `ApiBase` + `ApiKey` + `ModelId` + `ApiStyle` |
| `ApiBase` / `ApiKey` / `ModelId` | validated wire config newtypes (`try_new`) |
| `ApiStyle` | `responses` \| `completions` |
| `HistoryProjector` | optional strategy **port** (product implements) |
| `PassThrough` | default projector (no filter) |
| `OpenAiCompatRuntime` | mechanism: project → encode → stream |

Products own env vars and history policy, e.g.:

```rust
OpenAiCompatRuntime::new(cfg).with_projector(Arc::new(MyChatTextOnly));
```

```bash
cargo test -p phi-ext-llm
```
