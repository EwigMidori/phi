//! Agent run contract — not the provider loop.
//!
//! - Inputs: [`TurnRequest`]
//! - Outputs: [`AgentEvent`] stream (Data Stream–aligned)
//! - Tool execution and any permission/approval policy live in the `AgentRuntime`
//!   **adapter** (product), not in phi-kernel.
//! - [`AgentEvent::ToolApprovalRequired`] is a stream observation shape only, not a
//!   policy engine.
//! - Tool-result outcome is [`ToolResultStatus`] on the event; `output` is opaque.
//! - Prefix material ([`AgentPrefix`]) is **session/binding** owned; Turn carries a
//!   read-only snapshot. Turn-owned mechanics: [`ToolCallSealPolicy`], cancel, history.
//! - Ban: `stream(text) -> text` as the stable public boundary
//! - No Handout / tree / graph types here

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{MessageContent, TailState};
use crate::ids::{JobId, ModelResponseId, SessionId};

// ── Turn history projection ────────────────────────────────────────────────

/// One stored turn row in transcript order. The model context is the full
/// interleaved sequence (user / assistant / tool / reasoning rows), so
/// [`TurnRequest::history`] carries a single ordered [`Vec`] — no separate
/// dialogue / tool projections to reassemble.
///
/// `input` / `output` stay opaque `Value`s (kernel discipline: never sniffed).
///
/// **Reasoning is a sibling row**, not a field on [`TurnItem::Assistant`]
/// (Grok / Responses-API aligned):
/// - preserves interleaved order `[reasoning, tool, reasoning, …, assistant]`
/// - allows N parallel reasoning items without last-write-wins on one string
/// - products decide whether to re-send reasoning rows to the provider
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TurnItem {
    #[serde(rename_all = "camelCase")]
    User { content: MessageContent },
    /// Model chain-of-thought / reasoning summary for this span of the turn.
    /// Sits **before** (or between tool rows preceding) the answering
    /// [`TurnItem::Assistant`] — not nested inside it.
    #[serde(rename_all = "camelCase")]
    Reasoning { content: String },
    #[serde(rename_all = "camelCase")]
    Assistant { content: String },
    #[serde(rename_all = "camelCase")]
    ToolCall {
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        input: ToolArguments,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        output: Value,
        status: ToolResultStatus,
    },
    /// Provider-owned replay material, stored with its response group.
    Continuation { continuation: ProviderContinuation },
    /// Materialized response group. Never nested in persisted response rows.
    ModelResponse { response: ModelResponse },
}

/// Exact arguments emitted by the provider, including malformed JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolArguments(String);

impl ToolArguments {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn parse(&self) -> Result<Value, String> {
        serde_json::from_str(&self.0).map_err(|error| format!("invalid tool arguments: {error}"))
    }
    pub fn observation(&self) -> Value {
        self.parse()
            .unwrap_or_else(|_| Value::String(self.0.clone()))
    }
}
impl From<Value> for ToolArguments {
    fn from(value: Value) -> Self {
        Self(value.to_string())
    }
}
impl std::fmt::Display for ToolArguments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderContinuation {
    pub scope: String,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub id: ModelResponseId,
    pub rows: Vec<crate::transcript::TranscriptRow>,
    pub continuation: Option<ProviderContinuation>,
    /// False for interrupted visible text; incomplete calls are never included.
    pub complete: bool,
}

// ── Tools / tool-result status ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    /// Catalog name — same type as event/ledger [`ToolName`] (do not reintroduce bare `String`).
    pub name: ToolName,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// Sole outcome authority for a tool result. Parallel field on events/records —
/// never derived by reading keys on opaque `output` JSON.
///
/// **No [`Default`]** — callers must pick an explicit status (`Ok` is not an
/// implicit fallback). Aligns with AGENTS.md / README: outcome is explicit
/// authority (same stance as [`ToolCallSealPolicy`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, strum::AsRefStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ToolResultStatus {
    Ok,
    Denied,
    Error,
    Ask,
    Incomplete,
}

/// How the pump closes tool calls that never received a [`AgentEvent::ToolResult`].
///
/// **Opt-in mechanism, not a default policy.** The kernel's default posture is
/// [`Self::LeaveOpen`] — no auto-seal. The "every open call gets a terminal row"
/// closed-record invariant is a **product choice**: products that want it opt
/// into [`Self::SealOnStreamEnd`] / [`Self::SealAlways`].
///
/// **Turn / pump mechanism** (not model-prefix material). No [`Default`] — product must
/// choose explicitly via [`ToolCallSealSource`] (resolved per generation, not frozen
/// as a lone directory field).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallSealPolicy {
    /// Seal Incomplete when the agent stream ends; leave open on stop/cancel abort.
    SealOnStreamEnd,
    /// Seal Incomplete on stream end **and** stop/cancel abort.
    SealAlways,
    /// Never auto-seal; product/ext owns open-call cleanup.
    LeaveOpen,
}

impl ToolCallSealPolicy {
    /// `aborting` = cooperative stop or cancel-epoch stale before outcome.
    #[must_use]
    pub fn should_seal(self, aborting: bool) -> bool {
        match self {
            Self::LeaveOpen => false,
            Self::SealAlways => true,
            Self::SealOnStreamEnd => !aborting,
        }
    }
}

/// Resolves open-tool seal policy for a session (per generation call site).
///
/// Ownership is composition / product policy — not a constructor-frozen Copy on
/// [`crate::send_queue::SessionDirectory`] alone. [`TurnRequest::tool_call_seal`] is
/// the snapshot for one run.
pub trait ToolCallSealSource: Send + Sync {
    fn tool_call_seal_for(&self, session_id: &SessionId) -> ToolCallSealPolicy;
}

/// Fixed seal policy for all sessions (tests / simple products). Explicit — no default.
#[derive(Clone, Copy, Debug)]
pub struct FixedToolCallSeal(pub ToolCallSealPolicy);

impl ToolCallSealSource for FixedToolCallSeal {
    fn tool_call_seal_for(&self, _session_id: &SessionId) -> ToolCallSealPolicy {
        self.0
    }
}

// ── Agent prefix (session/binding owned; Turn carries a snapshot) ──────────

string_newtype! {
    /// Stable skill id in the index (not a filesystem path, not body text).
    pub struct SkillSlug;
    ordered
}

string_newtype! {
    /// Index-only skill blurb. Must not carry SKILL.md body.
    pub struct SkillDesc
}

string_newtype! {
    /// Provider-side tool-call **correlation key** (not a document / message PK).
    /// Opaque label from the model adapter; not UUID-generated by the kernel.
    pub struct ToolCallId
}

string_newtype! {
    /// Tool catalog name — the type of [`ToolSpec::name`] and tool events. Not a document identity.
    pub struct ToolName
}

/// One named block of system/preamble text.
///
/// Rendered by [`AgentPrefix::render_preamble`] as
/// `<{name}>{content}</{name}>` (sections joined with `\n`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreambleSection {
    pub name: String,
    pub content: String,
}

impl PreambleSection {
    #[must_use]
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            content: content.into(),
        }
    }

    /// `<name>content</name>`
    #[must_use]
    pub fn render(&self) -> String {
        format!("<{n}>{c}</{n}>", n = self.name, c = self.content)
    }
}

/// Session/agent **binding** material for the model prefix.
///
/// Not turn-owned. [`TurnRequest`] carries a read-only snapshot for one `run`.
/// Changing these fields across turns can bust provider prefix cache and dominate
/// cost at large contexts — product should treat updates as explicit binding changes.
///
/// - [`Self::preamble`]: ordered named sections (stable prefix text)
/// - [`Self::tools`]: tool schemas (stable prefix)
/// - [`Self::skill_index`]: skill **catalog** only (`SkillSlug` → short [`SkillDesc`]);
///   bodies must not live here
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPrefix {
    pub preamble: Vec<PreambleSection>,
    pub tools: Vec<ToolSpec>,
    /// `BTreeMap` so equal catalogs serialize/iterate in a stable order (prefix-friendly).
    pub skill_index: BTreeMap<SkillSlug, SkillDesc>,
}

impl AgentPrefix {
    /// Empty tools/skills; optional preamble sections.
    #[must_use]
    pub fn baseline_chat(preamble: Vec<PreambleSection>) -> Self {
        Self {
            preamble,
            tools: Vec::new(),
            skill_index: BTreeMap::new(),
        }
    }

    /// Render preamble sections as `<name>content</name>`, joined by newlines.
    #[must_use]
    pub fn render_preamble(&self) -> String {
        self.preamble
            .iter()
            .map(PreambleSection::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── Cancel / request ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct TurnCancel {
    state: Arc<TurnCancelState>,
    ancestors: Vec<Arc<TurnCancelState>>,
}

#[derive(Debug, Default)]
struct TurnCancelState {
    cancelled: AtomicBool,
    changed: tokio::sync::Notify,
}

impl TurnCancelState {
    async fn cancelled(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

impl TurnCancel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parent cancellation reaches this child and its descendants. Cancelling
    /// the child never cancels its parent or siblings. No relay task is spawned.
    #[must_use]
    pub fn child(&self) -> Self {
        let mut ancestors = self.ancestors.clone();
        ancestors.push(self.state.clone());
        Self {
            state: Arc::default(),
            ancestors,
        }
    }

    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        self.state.changed.notify_waiters();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        std::iter::once(&self.state)
            .chain(&self.ancestors)
            .any(|state| state.cancelled.load(Ordering::SeqCst))
    }

    /// Wake every waiter when this token or an ancestor is cancelled, including
    /// callers arriving afterwards. Dropping the future removes its waiters.
    pub async fn cancelled(&self) {
        let waiters: Vec<_> = std::iter::once(&self.state)
            .chain(&self.ancestors)
            .map(|state| Box::pin(state.cancelled()))
            .collect();
        futures::future::select_all(waiters).await;
    }
}

/// Input to [`AgentRuntime::run`].
///
/// - [`Self::history`]: full interleaved turn history (user / assistant / tool rows)
/// - [`Self::prefix`]: snapshot of binding prefix (not "this turn's policy")
/// - [`Self::tool_call_seal`]: this generation's pump seal policy
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub session_id: SessionId,
    pub job_id: JobId,
    pub history: Vec<TurnItem>,
    /// Transient material, separate from durable history and the stable prefix.
    pub tail_state: Option<TailState>,
    pub prefix: AgentPrefix,
    pub tool_call_seal: ToolCallSealPolicy,
    pub cancel: TurnCancel,
}

impl TurnRequest {
    /// Materialize transient tail state after host history projection, without
    /// mutating this request or its durable-history snapshot.
    pub fn materialize_history(
        &self,
        mut projected: Vec<TurnItem>,
    ) -> crate::error::Result<Vec<TurnItem>> {
        if let Some(tail) = &self.tail_state {
            tail.materialize(&mut projected)?;
        }
        Ok(projected)
    }
}

// ── Usage (provider metering observation) ──────────────────────────────────

/// Provider-reported token usage for one generation span.
///
/// **Observation only** — not a [`TurnItem`], never estimated by the kernel.
/// Missing fields stay [`None`]; do not invent `total` from partial sums.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss_tokens: Option<u64>,
}

impl Usage {
    #[must_use]
    pub fn new(
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        total_tokens: Option<u64>,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens: None,
            cache_miss_tokens: None,
        }
    }

    #[must_use]
    pub fn with_cache(
        mut self,
        cached_tokens: Option<u64>,
        cache_miss_tokens: Option<u64>,
    ) -> Self {
        self.cached_tokens = cached_tokens;
        self.cache_miss_tokens = cache_miss_tokens;
        self
    }

    /// True when the provider reported no counters.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens.is_none()
            && self.completion_tokens.is_none()
            && self.total_tokens.is_none()
            && self.cached_tokens.is_none()
            && self.cache_miss_tokens.is_none()
    }
}

// ── AgentEvent ─────────────────────────────────────────────────────────────

/// Tool correlation uses [`ToolCallId`] / [`ToolName`] (not document PKs).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    ResponseStarted {
        response_id: ModelResponseId,
        assistant_message_id: crate::MessageId,
    },
    ModelResponseCompleted {
        response: ModelResponse,
    },
    #[serde(rename_all = "camelCase")]
    TextDelta {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningDelta {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        response_id: ModelResponseId,
        tool_call_id: ToolCallId,
        output: Value,
        /// Sole outcome authority. Opaque `output` must not re-encode this.
        /// Required on the wire — no serde default (same as no [`Default`] on the enum).
        status: ToolResultStatus,
    },
    #[serde(rename_all = "camelCase")]
    ToolApprovalRequired {
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        input: Value,
    },
    /// Provider token usage (notice path only; not transcript).
    #[serde(rename_all = "camelCase")]
    Usage {
        usage: Usage,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    Finished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Unknown {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
    },
}

pub type AgentEventStream = Pin<Box<dyn Stream<Item = Result<AgentEvent, String>> + Send>>;

#[async_trait]
pub trait AgentRunLifecycle: Send + Sync {
    async fn close_and_join(&self) -> Result<(), String>;
}

/// Optional capability to stop generation while reading the current response's
/// remaining usage. After `begin`, subsequent polls may yield only Usage and a
/// terminal event: no tool execution, next request, text or response commits.
/// The consumer bounds the wait and still owns `close_and_join`.
pub trait ResponseUsageDrain: Send + Sync {
    fn begin(&self);
}

/// A pull-driven run. No further effect may begin until the consumer requests
/// the next event after durably committing the previous event.
pub struct AgentRun {
    stream: Option<AgentEventStream>,
    lifecycle: Option<Arc<dyn AgentRunLifecycle>>,
    usage_drain: Option<Arc<dyn ResponseUsageDrain>>,
}

impl AgentRun {
    /// A complete text response, useful to hosts providing deterministic agents.
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        let response_id = ModelResponseId::generate();
        let assistant_message_id = crate::MessageId::generate();
        let response = ModelResponse {
            id: response_id.clone(),
            rows: vec![crate::TranscriptRow::new(
                assistant_message_id.clone(),
                TurnItem::Assistant {
                    content: text.clone(),
                },
            )],
            continuation: None,
            complete: true,
        };
        Self::new(Box::pin(futures::stream::iter([
            Ok(AgentEvent::ResponseStarted {
                response_id,
                assistant_message_id,
            }),
            Ok(AgentEvent::TextDelta { text }),
            Ok(AgentEvent::ModelResponseCompleted { response }),
            Ok(AgentEvent::Finished { reason: None }),
        ])))
    }
    pub fn new(stream: AgentEventStream) -> Self {
        Self {
            stream: Some(stream),
            lifecycle: None,
            usage_drain: None,
        }
    }
    pub fn with_lifecycle(stream: AgentEventStream, lifecycle: Arc<dyn AgentRunLifecycle>) -> Self {
        Self {
            stream: Some(stream),
            lifecycle: Some(lifecycle),
            usage_drain: None,
        }
    }
    pub fn with_usage_drain(mut self, control: Arc<dyn ResponseUsageDrain>) -> Self {
        self.usage_drain = Some(control);
        self
    }

    pub fn usage_drain(&self) -> Option<Arc<dyn ResponseUsageDrain>> {
        self.usage_drain.clone()
    }
    /// Transform the pull stream without replacing its shutdown owner. Even if
    /// the transformed stream ends early, callers must await `close_and_join`
    /// to drop the upstream stream and finish the original run's cleanup.
    pub fn map_stream(
        mut self,
        transform: impl FnOnce(AgentEventStream) -> AgentEventStream,
    ) -> Self {
        self.stream = self.stream.take().map(transform);
        self
    }
    /// Observe events without changing pull order or losing the run's shutdown owner.
    pub fn inspect_events(
        mut self,
        inspect: impl FnMut(&Result<AgentEvent, String>) + Send + 'static,
    ) -> Self {
        use futures::StreamExt;
        self.stream = self
            .stream
            .take()
            .map(|stream| Box::pin(stream.inspect(inspect)) as AgentEventStream);
        self
    }

    /// Hosts may commit an observed fact before it reaches the consumer. A failed commit
    /// becomes a stream error; the run retains its original shutdown owner.
    pub fn try_inspect_events(
        mut self,
        mut inspect: impl FnMut(&AgentEvent) -> Result<(), String> + Send + 'static,
    ) -> Self {
        use futures::StreamExt;
        self.stream = self.stream.take().map(|stream| {
            Box::pin(stream.map(move |event| {
                if let Ok(value) = &event {
                    inspect(value)?;
                }
                event
            })) as AgentEventStream
        });
        self
    }

    pub async fn next(&mut self) -> Option<Result<AgentEvent, String>> {
        use futures::StreamExt;
        match self.stream.as_mut() {
            Some(stream) => stream.next().await,
            None => None,
        }
    }
    pub async fn close_and_join(&mut self) -> Result<(), String> {
        self.stream.take();
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.close_and_join().await?;
        }
        self.lifecycle.take();
        self.usage_drain.take();
        Ok(())
    }
}

#[async_trait]
pub trait AgentRuntime: Send + Sync {
    async fn run(&self, request: TurnRequest) -> Result<AgentRun, String>;
}

// ── Oneshot text complete (mechanism, not product labeling policy) ─────────

/// Bare text completion: **string in → string out**.
///
/// - **No** product binding prefix, tools, or session transcript side effects.
/// - **Not** a substitute for [`AgentRuntime`] dialogue turns.
/// - Callers own any prompt policy (e.g. product session-name materials).
///
/// Implementations typically share one LLM client object with [`AgentRuntime`]
/// (same wire stack, different message — not a second HTTP client).
#[async_trait]
pub trait OneshotText: Send + Sync {
    /// Complete `input` and return the model text (concatenated deltas).
    async fn complete(&self, input: &str) -> Result<String, String>;
}

// ── Agent prefix source (binding / composition, not Turn) ──────────────────

/// Resolves model-prefix material for a session.
///
/// Ownership is session/agent binding (composition root), not the generation turn.
pub trait AgentPrefixSource: Send + Sync {
    fn prefix_for(&self, session_id: &SessionId) -> AgentPrefix;
}

/// Empty tools/skills and empty preamble sections.
#[derive(Clone, Debug, Default)]
pub struct EmptyAgentPrefix;

impl AgentPrefixSource for EmptyAgentPrefix {
    fn prefix_for(&self, _session_id: &SessionId) -> AgentPrefix {
        AgentPrefix::baseline_chat(Vec::new())
    }
}

/// Fixed prefix for all sessions (tests / simple products).
#[derive(Clone, Debug)]
pub struct FixedAgentPrefix(pub AgentPrefix);

impl AgentPrefixSource for FixedAgentPrefix {
    fn prefix_for(&self, _session_id: &SessionId) -> AgentPrefix {
        self.0.clone()
    }
}

// ── Turn materials (prepare) vs runtime (execute) ──────────────────────────

/// Assembles a [`TurnRequest`] for one generation (prefix + open-tool seal snapshots).
///
/// Pump collaboration is **materials + runtime**, not three peer ports.
/// Implementations may delegate to [`AgentPrefixSource`] / [`ToolCallSealSource`]
/// ([`SourcesTurnMaterials`]) or supply a custom prepare path.
pub trait TurnMaterials: Send + Sync {
    fn prepare(
        &self,
        session_id: &SessionId,
        job_id: JobId,
        history: Vec<TurnItem>,
        cancel: TurnCancel,
    ) -> TurnRequest;
}

/// Default materials: resolve prefix + open-tool policy per call, fill [`TurnRequest`].
pub struct SourcesTurnMaterials {
    pub prefix: Arc<dyn AgentPrefixSource>,
    pub tool_call_seal: Arc<dyn ToolCallSealSource>,
}

impl SourcesTurnMaterials {
    #[must_use]
    pub fn new(
        prefix: Arc<dyn AgentPrefixSource>,
        tool_call_seal: Arc<dyn ToolCallSealSource>,
    ) -> Self {
        Self {
            prefix,
            tool_call_seal,
        }
    }
}

impl TurnMaterials for SourcesTurnMaterials {
    fn prepare(
        &self,
        session_id: &SessionId,
        job_id: JobId,
        history: Vec<TurnItem>,
        cancel: TurnCancel,
    ) -> TurnRequest {
        TurnRequest {
            session_id: session_id.clone(),
            job_id,
            history,
            tail_state: None,
            prefix: self.prefix.prefix_for(session_id),
            tool_call_seal: self.tool_call_seal.tool_call_seal_for(session_id),
            cancel,
        }
    }
}

/// Constructor-injected plumbing: **execute** + **prepare** (not a domain aggregate).
///
/// - [`Self::agent`][]: [`AgentRuntime`]
/// - [`Self::materials`][]: [`TurnMaterials`] (typically [`SourcesTurnMaterials`])
///
/// Prefer this over passing three loose ports through the pump.
#[derive(Clone)]
pub struct AgentPorts {
    pub agent: Arc<dyn AgentRuntime>,
    pub materials: Arc<dyn TurnMaterials>,
}

impl AgentPorts {
    #[must_use]
    pub fn new(agent: Arc<dyn AgentRuntime>, materials: Arc<dyn TurnMaterials>) -> Self {
        Self { agent, materials }
    }

    /// Build from the two sources via [`SourcesTurnMaterials`].
    #[must_use]
    pub fn from_sources(
        agent: Arc<dyn AgentRuntime>,
        prefix: Arc<dyn AgentPrefixSource>,
        tool_call_seal: Arc<dyn ToolCallSealSource>,
    ) -> Self {
        Self::new(
            agent,
            Arc::new(SourcesTurnMaterials::new(prefix, tool_call_seal)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentPrefix, EmptyAgentPrefix, FixedToolCallSeal, PreambleSection, SkillDesc, SkillSlug,
        SourcesTurnMaterials, ToolCallSealPolicy, TurnCancel, TurnMaterials,
    };
    use crate::ids::{JobId, SessionId};
    use std::sync::Arc;

    #[test]
    fn tool_call_seal_policy_should_seal_matrix() {
        // SealOnStreamEnd: seal when stream ends cleanly; leave open on abort.
        assert!(ToolCallSealPolicy::SealOnStreamEnd.should_seal(false));
        assert!(!ToolCallSealPolicy::SealOnStreamEnd.should_seal(true));
        // SealAlways: always seal.
        assert!(ToolCallSealPolicy::SealAlways.should_seal(false));
        assert!(ToolCallSealPolicy::SealAlways.should_seal(true));
        // LeaveOpen: never auto-seal.
        assert!(!ToolCallSealPolicy::LeaveOpen.should_seal(false));
        assert!(!ToolCallSealPolicy::LeaveOpen.should_seal(true));
    }

    #[test]
    fn preamble_sections_render_as_named_tags() {
        let prefix = AgentPrefix::baseline_chat(vec![
            PreambleSection::new("system", "You are helpful."),
            PreambleSection::new("style", "Be brief."),
        ]);
        assert_eq!(
            prefix.render_preamble(),
            "<system>You are helpful.</system>\n<style>Be brief.</style>"
        );
    }

    #[test]
    fn skill_index_is_slug_to_desc_map() {
        let mut prefix = AgentPrefix::default();
        prefix
            .skill_index
            .insert(SkillSlug::new("search"), SkillDesc::new("web search"));
        prefix
            .skill_index
            .insert(SkillSlug::new("calc"), SkillDesc::new("arithmetic"));
        let keys: Vec<_> = prefix.skill_index.keys().map(SkillSlug::as_str).collect();
        assert_eq!(keys, ["calc", "search"]); // BTreeMap order
        assert_eq!(
            prefix
                .skill_index
                .get(&SkillSlug::new("search"))
                .unwrap()
                .as_str(),
            "web search"
        );
    }

    #[test]
    fn sources_turn_materials_prepare_fills_request() {
        let materials = SourcesTurnMaterials::new(
            Arc::new(EmptyAgentPrefix),
            Arc::new(FixedToolCallSeal(ToolCallSealPolicy::SealAlways)),
        );
        let sid = SessionId::generate();
        let job_id = JobId::generate();
        let req = materials.prepare(&sid, job_id.clone(), Vec::new(), TurnCancel::new());
        assert_eq!(req.session_id, sid);
        assert_eq!(req.job_id, job_id);
        assert!(req.history.is_empty());
        assert!(req.prefix.tools.is_empty());
        assert_eq!(req.tool_call_seal, ToolCallSealPolicy::SealAlways);
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn child_cancellation_wakes_descendants_without_cancelling_parent_or_sibling() {
        let parent = TurnCancel::new();
        let child = parent.child();
        let sibling = parent.child();
        let grandchild = child.child();
        let child_wait = child.cancelled();
        let grandchild_wait = grandchild.cancelled();
        tokio::pin!(child_wait, grandchild_wait);
        assert!(futures::poll!(&mut child_wait).is_pending());
        assert!(futures::poll!(&mut grandchild_wait).is_pending());
        child.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            child_wait.await;
            grandchild_wait.await;
        })
        .await
        .unwrap();
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        assert!(child.child().is_cancelled());
        assert!(!parent.is_cancelled());
        assert!(!sibling.is_cancelled());
    }

    #[tokio::test]
    async fn parent_cancellation_wakes_all_levels_and_late_children() {
        let parent = TurnCancel::new();
        let child = parent.child();
        let grandchild = child.child();
        let child_wait = child.cancelled();
        let grandchild_wait = grandchild.cancelled();
        tokio::pin!(child_wait, grandchild_wait);
        assert!(futures::poll!(&mut child_wait).is_pending());
        assert!(futures::poll!(&mut grandchild_wait).is_pending());
        parent.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            child_wait.await;
            grandchild_wait.await;
            parent.child().cancelled().await;
        })
        .await
        .unwrap();
        assert!(parent.is_cancelled());
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
    }
}

#[cfg(test)]
mod run_inspection_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Lifecycle(Arc<AtomicUsize>);
    #[async_trait]
    impl AgentRunLifecycle for Lifecycle {
        async fn close_and_join(&self) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[tokio::test]
    async fn mapped_early_end_keeps_upstream_cleanup_and_drops_stream_before_join() {
        use futures::StreamExt;
        struct StreamGuard(Arc<AtomicBool>);
        impl Drop for StreamGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        struct CheckedLifecycle {
            dropped: Arc<AtomicBool>,
            closed: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl AgentRunLifecycle for CheckedLifecycle {
            async fn close_and_join(&self) -> Result<(), String> {
                assert!(self.dropped.load(Ordering::SeqCst));
                self.closed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicUsize::new(0));
        let polls = Arc::new(AtomicUsize::new(0));
        let upstream = futures::stream::unfold(
            (StreamGuard(dropped.clone()), polls.clone()),
            |state| async move {
                state.1.fetch_add(1, Ordering::SeqCst);
                Some((
                    Ok(AgentEvent::TextDelta {
                        text: "one event".into(),
                    }),
                    state,
                ))
            },
        );
        let mut run = AgentRun::with_lifecycle(
            Box::pin(upstream),
            Arc::new(CheckedLifecycle {
                dropped: dropped.clone(),
                closed: closed.clone(),
            }),
        )
        .map_stream(|stream| Box::pin(stream.take(1)));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            run.next().await,
            Some(Ok(AgentEvent::TextDelta { .. }))
        ));
        assert!(run.next().await.is_none());
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        run.close_and_join().await.unwrap();
        run.close_and_join().await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(run.next().await.is_none());
    }
    #[tokio::test]
    async fn inspection_is_pull_driven_and_preserves_shutdown() {
        let observed = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let count = observed.clone();
        let mut run = AgentRun::with_lifecycle(
            Box::pin(futures::stream::iter([
                Ok(AgentEvent::Usage {
                    usage: Usage::new(Some(12), None, None),
                }),
                Err("failed".into()),
            ])),
            Arc::new(Lifecycle(closed.clone())),
        )
        .inspect_events(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(observed.load(Ordering::SeqCst), 0);
        assert!(matches!(
            run.next().await,
            Some(Ok(AgentEvent::Usage { .. }))
        ));
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        assert!(matches!(run.next().await, Some(Err(message)) if message == "failed"));
        run.close_and_join().await.unwrap();
        run.close_and_join().await.unwrap();
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(observed.load(Ordering::SeqCst), 2);
        assert!(run.next().await.is_none());
    }

    #[tokio::test]
    async fn fallible_inspection_reports_host_failure_and_keeps_shutdown() {
        let closed = Arc::new(AtomicUsize::new(0));
        let mut run = AgentRun::with_lifecycle(
            Box::pin(futures::stream::iter([Ok(AgentEvent::Usage {
                usage: Usage::new(Some(12), None, None),
            })])),
            Arc::new(Lifecycle(closed.clone())),
        )
        .try_inspect_events(|_| Err("host write failed".into()));
        assert!(matches!(run.next().await, Some(Err(error)) if error == "host write failed"));
        run.close_and_join().await.unwrap();
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(run.next().await.is_none());
    }
}
