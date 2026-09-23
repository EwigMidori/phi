# Gemini GenerateContent v1beta fixtures

Synthetic protocol fixtures checked against Google's native REST reference on 2026-09-22. They are not captured account traffic, and their base64 signatures are inert test values. Tests verify response/tool behavior and replay, not our serde attribute shapes.

Sources:

- https://ai.google.dev/api/generate-content — both endpoints, Content/Part, FunctionCall optional id, FunctionResponse object result, terminal reasons, usage metadata, parametersJsonSchema.
- https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures — preserve signed parts, first call in a group, empty-text late signatures, current versus previous user turns, calls before grouped results.
- https://ai.google.dev/gemini-api/docs/generate-content/thinking — explicit level or token budget; dynamic budget -1 and disabled budget 0 have distinct meanings.
- https://raw.githubusercontent.com/googleapis/python-genai/main/google/genai/chats.py — reviewed stream handling appends each returned content in receive order and preserves its parts. It does not treat repeated text as cumulative snapshots. We do not copy its automatic tool loop.

The adapter never enables incremental function-argument extensions. Complete native function-call parts are accepted; unknown partial-argument fields fail before execution. Text fragments become contiguous visible rows while original parts remain unmerged in continuation. SSE EOF is accepted only after a valid STOP and complete framing. A STOP does not discard later signature/usage frames. The complete JSON endpoint is not transformed into SSE.

Nullable-response regression cases were checked on 2026-09-23 against [OpenCode v2's Gemini adapter](https://github.com/anomalyco/opencode/blob/43f1dad8e1c2481373a1c6376352f9b6d5f3dc72/packages/ai/src/protocols/gemini.ts), its `test/provider/gemini.test.ts` cases, and [ProtoJSON null semantics](https://protobuf.dev/programming-guides/json/#null-values). Optional null fields remain unset; null finish reasons do not complete a response, reset an earlier STOP, or authorize tool execution. Raw signed parts are preserved, including nulls inside tool arguments. Null required signatures still fail. Terminal failures retain a bounded enum token in the diagnostic and never seal pending tool calls.
