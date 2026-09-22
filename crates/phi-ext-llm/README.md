# phi-ext-llm

OpenAI-compatible HTTP/SSE → kernel `AgentRuntime`.

| API | Role |
|-----|------|
| `LlmConfig` | `ApiBase` + `ApiKey` + `ModelId` + `ApiStyle` |
| `ApiBase` / `ApiKey` / `ModelId` | validated wire config newtypes (`try_new`) |
| `ApiStyle` | `responses` \| `completions` |
| `ReasoningConfig` | explicit dialect + provider default / disabled / enabled / exact effort |
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

`with_reasoning(ReasoningConfig { dialect, mode })` selects OpenAI, DeepSeek,
Gemini's OpenAI compatibility endpoint, Qwen, SiliconFlow, or OpenRouter wire
behavior. The host determines the endpoint and model capabilities; this crate
never guesses them from a model name. `ProviderDefault` omits control parameters
and still retains the selected dialect's continuation handling. Effort values
are never converted to a nearby level, and `Enabled` is accepted only for real
toggle protocols. Gemini, Qwen and SiliconFlow currently require Completions.
Budget controls and native Gemini/Anthropic APIs are outside this adapter.

Completions preserves Gemini thought signatures and OpenRouter reasoning details
in `ProviderContinuation`. DeepSeek and SiliconFlow replay their reasoning text
when thinking is not explicitly disabled, including provider-default requests.
New continuation scopes include the dialect version; existing Responses v1
scopes still replay at the same endpoint/API/model. Changing effort does not
invalidate continuation. Visible-text projection cannot overwrite opaque state.

Protocol fixtures were reviewed on 2026-09-22 against the official
[OpenAI reasoning guide](https://developers.openai.com/api/docs/guides/reasoning),
[DeepSeek thinking guide](https://api-docs.deepseek.com/guides/thinking_mode/),
[Gemini compatibility guide](https://ai.google.dev/gemini-api/docs/openai) and
[signature examples](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures),
[Qwen guide](https://help.aliyun.com/zh/model-studio/deep-thinking),
[SiliconFlow API](https://docs.siliconflow.cn/docs/api/chat-completions-post), and
[OpenRouter reasoning guide](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).

`with_tool_images` injects a host `ToolOutputImages` interpreter for opaque tool outputs. Images are projected into request-only, labelled user-role material after all adjacent tool replies (including Completions correlation constraints); persisted history is unchanged. The existing image service transfers actual bytes or provider file references. Text-only policies emit an explicit cannot-inspect notice instead of sending unsupported images.
