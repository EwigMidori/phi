# phi-ext-llm

OpenAI-compatible HTTP/SSE → kernel `AgentRuntime`.

| API | Role |
|-----|------|
| `LlmConfig` | `ApiBase` + `ApiKey` + `ModelId` + `ApiStyle` |
| `ApiBase` / `ApiKey` / `ModelId` | validated wire config newtypes (`try_new`) |
| `ApiStyle` | `responses` \| `completions` |
| `HistoryProjector` | optional strategy **port** (product implements) |
| `PassThrough` | default projector (no filter) |
| `OpenAiCompatRuntime` | mechanism: project → provider response → durable batch → tools → continue |

Products own env vars and history policy, e.g.:

```rust
OpenAiCompatRuntime::new(cfg).with_projector(Arc::new(MyChatTextOnly));
```

```bash
cargo test -p phi-ext-llm
```

`with_tools(Arc<ToolRegistry>)` injects executable bindings. The turn prefix owns
which tools are enabled; empty-tool turns never execute a model tool request.
The returned `AgentRun` is pull-driven: each response/result must be committed
before requesting the next event, and every exit must await `close_and_join`.

Both Chat Completions and Responses support streamed function arguments, multiple
serial tools, explicit error results, and continuation. Responses reasoning
replay material is scoped to the provider, protocol, and model and must survive
transcript persistence. A matching Responses scope replays the opaque payload;
any other provider/protocol/model gets the portable visible rows. Provider response
completion is distinct from the generation's final `Finished` event.
