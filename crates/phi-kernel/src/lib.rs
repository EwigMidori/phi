//! # phi-kernel
//!
//! Tree-optional **agent kernel** for [phi](https://github.com/ewigmidori/phi):
//!
//! | Area | Surface |
//! |------|---------|
//! | Contract | [`AgentRuntime`], [`TurnRequest`], [`AgentEvent`] |
//! | Generation | [`SendQueue`], [`SessionDirectory`], [`GenerationJob`] |
//! | History | [`Transcript`], [`InMemoryTranscript`] |
//! | Observe | [`KernelEvent`], [`EventBus`] |
//!
//! **Not in this crate:** session graph, fork/tombstone, Handout, product prefs, Rig/HTTP,
//! permission/approval policy engines (adapter-owned). `ToolApprovalRequired` is stream shape only.
//! Tree-agent mechanisms belong in `phi-ext-*`.
//!
//! **Naming:** use **SendQueue**, never "mailbox" (reserved for a future agent notify bus).
//!
//! **Generation commit:** stream batches use [`send_queue`] `EffectBatch` —
//! each transcript write then projected tool notice, then pure notices; terminal
//! jobs use `TurnOutcome` (see `send_queue::effect`).
//!
//! **Tool results:** outcome is [`ToolResultStatus`] only; `output` JSON is opaque to the kernel.
//!
//! **Prefix vs turn:** [`AgentPrefix`] is binding-owned; [`TurnRequest`] carries a snapshot.
//! Pump injects [`AgentPorts`] = [`AgentRuntime`] + [`TurnMaterials`] (prepare; typically
//! [`SourcesTurnMaterials`] over prefix + open-tool sources).

#![forbid(unsafe_code)]

/// Crate-private boilerplate for open string labels (see module docs).
#[macro_use]
mod string_newtype;

// Modules are private; crate root re-exports are the public surface.
mod agent;
mod error;
mod events;
mod ids;
mod send_queue;
mod transcript;

pub use agent::{
    AgentEvent, AgentEventStream, AgentPorts, AgentPrefix, AgentPrefixSource, AgentRuntime,
    EmptyAgentPrefix, FixedAgentPrefix, FixedToolCallSeal, PreambleSection, SkillDesc, SkillSlug,
    SourcesTurnMaterials, ToolCallId, ToolCallSealPolicy, ToolCallSealSource, ToolName,
    ToolResultStatus, ToolSpec, TurnCancel, TurnItem, TurnMaterials, TurnRequest, Usage,
};
pub use error::{KernelError, Result};
pub use events::{EventBus, KernelEvent};
pub use ids::{JobId, MessageId, SessionId};
pub use send_queue::{GenerationJob, SendQueue, SessionDirectory};
pub use transcript::{InMemoryTranscript, RecordResult, Transcript, TruncateResult};

pub const KERNEL_NAME: &str = "phi-kernel";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod integration {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::stream;

    use super::*;

    struct FixedTextAgent(&'static str);

    #[async_trait]
    impl AgentRuntime for FixedTextAgent {
        async fn run(&self, request: TurnRequest) -> std::result::Result<AgentEventStream, String> {
            assert!(
                !request.history.is_empty(),
                "history required for generation"
            );
            let text = self.0.to_owned();
            Ok(Box::pin(stream::iter([
                Ok(AgentEvent::TextDelta { text }),
                Ok(AgentEvent::Finished {
                    reason: Some("stop".into()),
                }),
            ])))
        }
    }

    struct ToolThenTextAgent;

    #[async_trait]
    impl AgentRuntime for ToolThenTextAgent {
        async fn run(
            &self,
            _request: TurnRequest,
        ) -> std::result::Result<AgentEventStream, String> {
            Ok(Box::pin(stream::iter([
                Ok(AgentEvent::ToolCall {
                    tool_call_id: ToolCallId::new("tc1"),
                    tool_name: ToolName::new("echo"),
                    input: serde_json::json!({"x": 1}),
                }),
                Ok(AgentEvent::ToolResult {
                    tool_call_id: ToolCallId::new("tc1"),
                    output: serde_json::json!({"echo": {"x": 1}}),
                    status: ToolResultStatus::Ok,
                }),
                Ok(AgentEvent::TextDelta {
                    text: "after-tool".into(),
                }),
                Ok(AgentEvent::Finished {
                    reason: Some("stop".into()),
                }),
            ])))
        }
    }

    fn bus() -> EventBus {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        tx
    }

    fn test_ports(agent: Arc<dyn AgentRuntime>) -> AgentPorts {
        AgentPorts::from_sources(
            agent,
            Arc::new(EmptyAgentPrefix),
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::LeaveOpen)),
        )
    }

    #[tokio::test]
    async fn send_twice_drain_two_assistants() {
        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();

        let dir = SessionDirectory::new(test_ports(Arc::new(FixedTextAgent("reply"))));
        dir.activate(&sid);
        let events = bus();

        let u1 = store.record_user(&sid, "one").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u1.message_id))
            .unwrap();
        let u2 = store.record_user(&sid, "two").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u2.message_id))
            .unwrap();

        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let history = store.load_turn_history(&sid).unwrap();
        let assistants: Vec<_> = history
            .iter()
            .filter_map(|t| match t {
                TurnItem::Assistant { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(assistants, ["reply", "reply"]);
        assert!(store.version().unwrap() >= 4);
    }

    #[tokio::test]
    async fn stop_skips_assistant_write() {
        use std::sync::Arc as StdArc;
        use tokio::sync::Notify;

        struct GateAgent {
            entered: StdArc<Notify>,
            release: StdArc<Notify>,
        }

        #[async_trait]
        impl AgentRuntime for GateAgent {
            async fn run(
                &self,
                _request: TurnRequest,
            ) -> std::result::Result<AgentEventStream, String> {
                let entered = self.entered.clone();
                let release = self.release.clone();
                Ok(Box::pin(stream::once(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(AgentEvent::TextDelta {
                        text: "late".into(),
                    })
                })))
            }
        }

        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();
        let entered = StdArc::new(Notify::new());
        let release = StdArc::new(Notify::new());
        let dir = SessionDirectory::new(test_ports(Arc::new(GateAgent {
            entered: entered.clone(),
            release: release.clone(),
        })));
        dir.activate(&sid);
        let events = bus();

        let u = store.record_user(&sid, "keep me").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u.message_id))
            .unwrap();

        let dir2 = dir.clone();
        let sid2 = sid.clone();
        let store2 = store.clone();
        let events2 = events.clone();
        let handle = tokio::spawn(async move {
            dir2.run_until_idle(&sid2, &store2, &events2).await.unwrap();
        });
        entered.notified().await;
        dir.stop(&sid);
        release.notify_one();
        handle.await.unwrap();

        let history = store.load_turn_history(&sid).unwrap();
        assert_eq!(history.len(), 1);
        assert!(matches!(&history[0], TurnItem::User { content } if content == "keep me"));
    }

    #[tokio::test]
    async fn truncate_from_then_edit_resend() {
        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();
        let dir = SessionDirectory::new(test_ports(Arc::new(FixedTextAgent("new-reply"))));
        dir.activate(&sid);
        let events = bus();

        let u1 = store.record_user(&sid, "one").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u1.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let u2 = store.record_user(&sid, "two").unwrap();
        let u2_id = u2.message_id.clone();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u2_id.clone()))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let before = store.load_turn_history(&sid).unwrap();
        assert_eq!(before.len(), 4); // U1 A1 U2 A2

        dir.stop(&sid);
        let _ = dir.cancel_pending(&sid, None);
        let cut = store.truncate_from(&sid, &u2_id).unwrap();
        assert_eq!(cut.removed_count, 2); // U2 + A2
        assert_eq!(store.load_turn_history(&sid).unwrap().len(), 2);

        let u2b = store.record_user(&sid, "two-edited").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u2b.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let history = store.load_turn_history(&sid).unwrap();
        assert_eq!(history.len(), 4);
        assert!(matches!(&history[0], TurnItem::User { content } if content == "one"));
        assert!(matches!(&history[2], TurnItem::User { content } if content == "two-edited"));
        assert!(matches!(&history[3], TurnItem::Assistant { content } if content == "new-reply"));
        assert!(!history
            .iter()
            .any(|t| matches!(t, TurnItem::User { content } if content == "two")));
    }

    #[tokio::test]
    async fn tool_call_is_recorded() {
        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();
        let dir = SessionDirectory::new(AgentPorts::from_sources(
            Arc::new(ToolThenTextAgent),
            Arc::new(FixedAgentPrefix(AgentPrefix {
                preamble: Vec::new(),
                tools: vec![ToolSpec {
                    name: "echo".into(),
                    description: "echo".into(),
                    parameters: None,
                }],
                skill_index: BTreeMap::default(),
            })),
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::LeaveOpen)),
        ));
        dir.activate(&sid);
        let events = bus();
        let u = store.record_user(&sid, "use tool").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let history = store.load_turn_history(&sid).unwrap();
        assert!(
            history
                .iter()
                .any(|t| matches!(t, TurnItem::Assistant { content } if content == "after-tool"))
        );
        // tool rows are part of turn history; version advanced past user+tools+assistant
        assert!(store.version().unwrap() >= 4);
    }

    #[tokio::test]
    async fn leave_open_posture_writes_no_incomplete_row() {
        // Sealing is opt-in: under the LeaveOpen default posture, a turn that ends
        // with an open tool leaves no fabricated ToolResult row in the history.
        struct ToolCallOnlyAgent;

        #[async_trait]
        impl AgentRuntime for ToolCallOnlyAgent {
            async fn run(
                &self,
                _request: TurnRequest,
            ) -> std::result::Result<AgentEventStream, String> {
                Ok(Box::pin(stream::iter([
                    Ok(AgentEvent::ToolCall {
                        tool_call_id: ToolCallId::new("tc1"),
                        tool_name: ToolName::new("search"),
                        input: serde_json::json!({"q": "x"}),
                    }),
                    Ok(AgentEvent::Finished {
                        reason: Some("stop".into()),
                    }),
                ])))
            }
        }

        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();
        let dir = SessionDirectory::new(AgentPorts::from_sources(
            Arc::new(ToolCallOnlyAgent),
            Arc::new(EmptyAgentPrefix),
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::LeaveOpen)),
        ));
        dir.activate(&sid);
        let events = bus();

        let u = store.record_user(&sid, "use tool").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let history = store.load_turn_history(&sid).unwrap();
        assert!(matches!(&history[0], TurnItem::User { .. }));
        assert!(matches!(&history[1], TurnItem::ToolCall { .. }));
        // No fabricated Incomplete result row under the default posture.
        assert!(!history.iter().any(|t| matches!(t, TurnItem::ToolResult { .. })));
    }

    #[tokio::test]
    async fn orchestrated_tool_rows_reach_next_turn_history() {
        // Regression: orchestration writes tool call + result rows between turns;
        // the next run_until_idle must deliver them to the model in transcript
        // order (interleaved, not dropped).
        use std::sync::Mutex;

        struct CaptureAgent {
            last: Arc<Mutex<Option<TurnRequest>>>,
        }

        #[async_trait]
        impl AgentRuntime for CaptureAgent {
            async fn run(
                &self,
                request: TurnRequest,
            ) -> std::result::Result<AgentEventStream, String> {
                *self.last.lock().unwrap() = Some(request);
                Ok(Box::pin(stream::iter([
                    Ok(AgentEvent::TextDelta {
                        text: "done".into(),
                    }),
                    Ok(AgentEvent::Finished {
                        reason: Some("stop".into()),
                    }),
                ])))
            }
        }

        let store = InMemoryTranscript::new();
        let sid = SessionId::generate();
        store.ensure_live(&sid).unwrap();
        let last = Arc::new(Mutex::new(None));
        let dir = SessionDirectory::new(test_ports(Arc::new(CaptureAgent {
            last: last.clone(),
        })));
        dir.activate(&sid);
        let events = bus();

        let u = store.record_user(&sid, "use tool").unwrap();
        let tc = ToolCallId::new("tc1");
        let name = ToolName::new("echo");
        // Orchestration writes the tool round-trip directly (external execution).
        store
            .record_tool_call(&sid, &tc, &name, &serde_json::json!({"x": 1}))
            .unwrap();
        store
            .record_tool_result(
                &sid,
                &tc,
                &name,
                &serde_json::json!({"echo": {"x": 1}}),
                ToolResultStatus::Ok,
            )
            .unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let req = last.lock().unwrap().clone().expect("agent ran");
        // Interleaved order preserved: user → tool call → tool result.
        assert!(matches!(&req.history[0], TurnItem::User { content } if content == "use tool"));
        assert!(matches!(
            &req.history[1],
            TurnItem::ToolCall { tool_call_id, tool_name, .. }
                if tool_call_id.as_str() == "tc1" && tool_name.as_str() == "echo"
        ));
        assert!(matches!(
            &req.history[2],
            TurnItem::ToolResult { tool_call_id, status: ToolResultStatus::Ok, .. }
                if tool_call_id.as_str() == "tc1"
        ));
    }

    #[test]
    fn scaffold_identity() {
        assert_eq!(KERNEL_NAME, "phi-kernel");
        assert!(!version().is_empty());
    }
}
