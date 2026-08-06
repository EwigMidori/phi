//! Product session driver: [`SessionHost`] + usage + notes.
//!
//! Composes `phi-ext-llm` runtime → [`SessionHost`] → kernel events + durable history.
//! **No UI. No process env.** Hosts (CLI, etc.) load config and call
//! [`TurnDriver::from_config`] / [`TurnDriver::unconfigured`].
//!
//! Side effects (host, meters) live on [`TurnDriver`] methods — not free functions.

use std::fmt;
use std::sync::Arc;

use phi_ext_llm::{ApiBase, ApiStyle, HistoryProjector, LlmConfig, ModelId, OpenAiCompatRuntime};

use crate::session::SessionHost;
use crate::{KernelEvent, TurnItem, Usage};

/// Context window size in tokens (denominator for usage chrome). Always ≥ 1.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct ContextWindowSize(u64);

impl ContextWindowSize {
    /// Product default when host does not specify a window.
    pub const DEFAULT: Self = Self(128_000);

    /// Reject zero.
    pub fn try_new(tokens: u64) -> Result<Self, String> {
        if tokens == 0 {
            return Err("context window must be ≥ 1".into());
        }
        Ok(Self(tokens))
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ContextWindowSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ContextWindowSize").field(&self.0).finish()
    }
}

impl fmt::Display for ContextWindowSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Result of [`TurnDriver::submit`] — shell clears the prompt only on [`Accepted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// User recorded + job enqueued; shell may clear prompt and start live view.
    Accepted,
    /// Busy / empty input — keep prompt text.
    Rejected,
    /// Config or host failure — keep prompt; note via [`TurnDriver::take_last_note`].
    Failed,
}

/// One non-blocking drain of the kernel bus (and host pump errors).
///
/// Pure application output — the shell maps this onto scrollback / notes.
#[derive(Debug, Default)]
pub struct TickResult {
    pub events: Vec<KernelEvent>,
    /// Receiver lagged — shell must resync durable history from [`TurnDriver::history`].
    pub lagged: bool,
    /// Pump / run failure this tick (message also parked in [`TurnDriver::take_last_note`]).
    pub pump_failed: bool,
    /// A generation ended this tick (done / stopped / error / pump fail).
    pub generation_finished: bool,
}

/// Channel identity for the status chrome (typed product fields, no paint formatting).
///
/// Reuses wire types [`ModelId`] / [`ApiBase`] / [`ApiStyle`] — no parallel string aliases.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub model: ModelId,
    /// [`None`] when the driver is unconfigured (no live channel).
    pub api_style: Option<ApiStyle>,
    pub api_base: ApiBase,
    pub context_window: ContextWindowSize,
}

/// Token usage for the status chrome (raw counts only).
#[derive(Debug, Clone)]
pub struct UsageInfo {
    pub last: Option<Usage>,
    pub session_prompt: u64,
    pub session_completion: u64,
    pub session_total: u64,
    pub context_window: ContextWindowSize,
}

impl Default for UsageInfo {
    fn default() -> Self {
        Self {
            last: None,
            session_prompt: 0,
            session_completion: 0,
            session_total: 0,
            context_window: ContextWindowSize::DEFAULT,
        }
    }
}

impl UsageInfo {
    /// Context filled on the last request (provider `prompt_tokens`, else `total_tokens`).
    #[must_use]
    pub fn context_used(&self) -> Option<u64> {
        let u = self.last.as_ref()?;
        u.prompt_tokens.or(u.total_tokens)
    }
}

/// Product meter: last job + session totals from provider [`Usage`] only.
#[derive(Debug, Default, Clone)]
struct UsageMeter {
    last: Option<Usage>,
    session_prompt: u64,
    session_completion: u64,
    session_total: u64,
}

impl UsageMeter {
    fn observe(&mut self, usage: Usage) {
        if let Some(p) = usage.prompt_tokens {
            self.session_prompt = self.session_prompt.saturating_add(p);
        }
        if let Some(c) = usage.completion_tokens {
            self.session_completion = self.session_completion.saturating_add(c);
        }
        if let Some(t) = usage.total_tokens {
            self.session_total = self.session_total.saturating_add(t);
        }
        self.last = Some(usage);
    }

    fn info(&self, context_window: ContextWindowSize) -> UsageInfo {
        UsageInfo {
            last: self.last.clone(),
            session_prompt: self.session_prompt,
            session_completion: self.session_completion,
            session_total: self.session_total,
            context_window,
        }
    }
}

/// Product session driver: kernel host + usage + notes. No view objects.
pub struct TurnDriver {
    host: Option<SessionHost>,
    config_error: Option<String>,
    model: ModelId,
    api_style: Option<ApiStyle>,
    api_base: ApiBase,
    context_window: ContextWindowSize,
    /// Last status (errors, approval, pump) — not injected into transcript.
    last_note: Option<String>,
    usage: UsageMeter,
}

impl TurnDriver {
    /// Build a live driver from host-supplied LLM config (no env reads).
    #[must_use]
    pub fn from_config(cfg: LlmConfig, context_window: ContextWindowSize) -> Self {
        let model = cfg.model.clone();
        let api_style = Some(cfg.api_style);
        let api_base = cfg.api_base.clone();
        // Product strategy: lean chat context (not owned by phi-ext-llm).
        let agent = Arc::new(
            OpenAiCompatRuntime::new(cfg).with_projector(Arc::new(ChatTextOnly)),
        );
        Self {
            host: Some(SessionHost::new(agent)),
            config_error: None,
            model,
            api_style,
            api_base,
            context_window,
            last_note: None,
            usage: UsageMeter::default(),
        }
    }

    /// Unconfigured driver — same failure shape as a missing host at submit time.
    #[must_use]
    pub fn unconfigured(error: impl Into<String>, context_window: ContextWindowSize) -> Self {
        // Placeholders are valid typed values for chrome; host is absent.
        let model = ModelId::try_new("unconfigured").expect("literal non-empty");
        let api_base = ApiBase::try_new("—").expect("literal non-empty");
        Self {
            host: None,
            config_error: Some(error.into()),
            model,
            api_style: None,
            api_base,
            context_window,
            last_note: None,
            usage: UsageMeter::default(),
        }
    }

    #[must_use]
    pub fn channel(&self) -> ChannelInfo {
        ChannelInfo {
            model: self.model.clone(),
            api_style: self.api_style,
            api_base: self.api_base.clone(),
            context_window: self.context_window,
        }
    }

    #[must_use]
    pub fn usage(&self) -> UsageInfo {
        self.usage.info(self.context_window)
    }

    #[must_use]
    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// Take the latest status note; clears the slot.
    pub fn take_last_note(&mut self) -> Option<String> {
        self.last_note.take()
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.host.as_ref().is_some_and(SessionHost::is_busy)
    }

    /// Durable transcript snapshot for the view to project.
    pub fn history(&self) -> Result<Vec<TurnItem>, String> {
        let Some(host) = self.host.as_ref() else {
            return Err("no LLM runtime configured".into());
        };
        host.history()
    }

    /// Submit user text via kernel transcript + SendQueue.
    ///
    /// Does not touch the view. Shell decides streaming gating and applies history.
    pub fn submit(&mut self, user: &str) -> SubmitOutcome {
        if self.is_busy() {
            return SubmitOutcome::Rejected;
        }
        let msg = user.trim();
        if msg.is_empty() {
            return SubmitOutcome::Rejected;
        }

        if let Some(err) = &self.config_error {
            self.last_note = Some(format!("config: {err}"));
            return SubmitOutcome::Failed;
        }

        let Some(host) = self.host.as_mut() else {
            self.last_note = Some("no LLM runtime configured".into());
            return SubmitOutcome::Failed;
        };

        match host.submit_user(msg) {
            Ok(()) => {
                // Surface history errors as notes; user is already recorded.
                if let Err(e) = host.history() {
                    self.last_note = Some(format!("history: {e}"));
                } else {
                    self.last_note = None;
                }
                SubmitOutcome::Accepted
            }
            Err(e) => {
                self.last_note = Some(e);
                SubmitOutcome::Failed
            }
        }
    }

    /// Drain kernel bus (+ host pump errors). Updates usage / notes only.
    pub fn tick(&mut self) -> TickResult {
        let Some(host) = self.host.as_mut() else {
            return TickResult::default();
        };
        let batch = host.poll_events();
        let mut result = TickResult {
            events: batch.events,
            lagged: batch.lagged,
            pump_failed: batch.pump_error.is_some(),
            generation_finished: false,
        };
        if let Some(err) = batch.pump_error {
            self.last_note = Some(err);
            result.generation_finished = true;
        }
        if batch.lagged {
            self.last_note = Some("event bus lagged — resynced from transcript".into());
        }
        for ev in &result.events {
            match ev {
                KernelEvent::GenerationUsage { usage, .. } => {
                    self.usage.observe(usage.clone());
                }
                KernelEvent::GenerationToolApprovalRequired {
                    tool_name, input, ..
                } => {
                    self.last_note = Some(format!("approval required: {tool_name} {input}"));
                }
                KernelEvent::GenerationAgentUnknown { kind, .. } => {
                    self.last_note = Some(format!("agent unknown event: {kind}"));
                }
                KernelEvent::GenerationDone { .. } => {
                    result.generation_finished = true;
                }
                KernelEvent::GenerationStopped { partial, .. } => {
                    result.generation_finished = true;
                    if !partial.is_empty() {
                        self.last_note =
                            Some("stopped (partial discarded from durable write)".into());
                    }
                }
                KernelEvent::GenerationError { message, .. } => {
                    result.generation_finished = true;
                    self.last_note = Some(format!("error: {message}"));
                }
                _ => {}
            }
        }
        result
    }
}

// ── Product history strategy (phi-code-core owns this policy) ───────────────

/// Lean multi-turn context: user + non-empty assistant only.
///
/// Reasoning / tool rows stay in transcript for the view; they are not re-sent on
/// this product’s default chat path. Other hosts inject their own [`HistoryProjector`].
struct ChatTextOnly;

impl HistoryProjector for ChatTextOnly {
    fn project(&self, history: &[TurnItem]) -> Vec<TurnItem> {
        history
            .iter()
            .filter(|item| match item {
                TurnItem::User { .. } => true,
                TurnItem::Assistant { content } => !content.is_empty(),
                TurnItem::Reasoning { .. }
                | TurnItem::ToolCall { .. }
                | TurnItem::ToolResult { .. } => false,
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::ContextWindowSize;

    #[test]
    fn context_window_rejects_zero() {
        assert!(ContextWindowSize::try_new(0).is_err());
        assert_eq!(ContextWindowSize::try_new(1).unwrap().get(), 1);
        assert_eq!(ContextWindowSize::DEFAULT.get(), 128_000);
    }
}
