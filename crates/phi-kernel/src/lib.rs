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

pub mod agent;
pub mod error;
pub mod events;
pub mod ids;
pub mod send_queue;
pub mod transcript;

pub use agent::{
    AgentEvent, AgentEventStream, AgentPorts, AgentPrefix, AgentPrefixSource, AgentRuntime,
    DialogueRole, DialogueTurn, EmptyAgentPrefix, FixedAgentPrefix, FixedToolCallSeal,
    PreambleSection, SkillDesc, SkillSlug, SourcesTurnMaterials, ToolCallId, ToolCallSealPolicy,
    ToolCallSealSource, ToolName, ToolResultStatus, ToolSpec, TurnCancel, TurnMaterials,
    TurnRequest,
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
                !request.dialogue.is_empty(),
                "dialogue required for generation"
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
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::SealOnStreamEnd)),
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

        let dialogue = store.load_dialogue(&sid).unwrap();
        let assistants: Vec<_> = dialogue
            .iter()
            .filter(|t| t.role == DialogueRole::Assistant)
            .map(|t| t.content.as_str())
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

        let dialogue = store.load_dialogue(&sid).unwrap();
        assert_eq!(dialogue.len(), 1);
        assert_eq!(dialogue[0].role, DialogueRole::User);
        assert_eq!(dialogue[0].content, "keep me");
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

        let before = store.load_dialogue(&sid).unwrap();
        assert_eq!(before.len(), 4); // U1 A1 U2 A2

        dir.stop(&sid);
        let _ = dir.cancel_pending(&sid, None);
        let cut = store.truncate_from(&sid, &u2_id).unwrap();
        assert_eq!(cut.removed_count, 2); // U2 + A2
        assert_eq!(store.load_dialogue(&sid).unwrap().len(), 2);

        let u2b = store.record_user(&sid, "two-edited").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u2b.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let dialogue = store.load_dialogue(&sid).unwrap();
        assert_eq!(dialogue.len(), 4);
        assert_eq!(dialogue[0].content, "one");
        assert_eq!(dialogue[2].content, "two-edited");
        assert_eq!(dialogue[3].content, "new-reply");
        assert!(!dialogue.iter().any(|t| t.content == "two"));
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
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::SealOnStreamEnd)),
        ));
        dir.activate(&sid);
        let events = bus();
        let u = store.record_user(&sid, "use tool").unwrap();
        dir.enqueue(&sid, GenerationJob::new(sid.clone(), u.message_id))
            .unwrap();
        dir.run_until_idle(&sid, &store, &events).await.unwrap();

        let dialogue = store.load_dialogue(&sid).unwrap();
        assert!(
            dialogue
                .iter()
                .any(|t| t.role == DialogueRole::Assistant && t.content == "after-tool")
        );
        // tool rows are not in dialogue projection; version advanced past user+tools+assistant
        assert!(store.version().unwrap() >= 4);
    }

    #[test]
    fn scaffold_identity() {
        assert_eq!(KERNEL_NAME, "phi-kernel");
        assert!(!version().is_empty());
    }
}
