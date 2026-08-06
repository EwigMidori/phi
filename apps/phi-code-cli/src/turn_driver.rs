//! Application / product layer for the CLI host.
//!
//! Env → `phi-ext-llm` runtime → [`SessionHost`] → kernel events + durable history.
//!
//! **No UI.** This module must not import `phi_code_ui`, ratatui, crossterm, or
//! format status-bar chrome. The shell projects [`TickResult`] / history into
//! scrollback and formats [`ChannelInfo`] / [`UsageInfo`] for paint.
//!
//! Side effects (env, host, meters) live on [`TurnDriver`] methods — not free
//! functions. Owned by the binary root; never re-home under `crate::shell`.

use std::sync::Arc;

use phi_code_core::{KernelEvent, SessionHost, TurnItem, Usage};
use phi_ext_llm::{ApiStyle, HistoryProjector, LlmConfig, OpenAiCompatRuntime};

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

/// Channel identity for the status chrome (raw product fields, no paint formatting).
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub model_id: String,
    pub api_style: String,
    pub api_base: String,
    pub context_window: u64,
}

/// Token usage for the status chrome (raw counts only).
#[derive(Debug, Clone, Default)]
pub struct UsageInfo {
    pub last: Option<Usage>,
    pub session_prompt: u64,
    pub session_completion: u64,
    pub session_total: u64,
    pub context_window: u64,
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

    fn info(&self, context_window: u64) -> UsageInfo {
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
    model_id: String,
    api_style: String,
    api_base: String,
    context_window: u64,
    /// Last status (errors, approval, pump) — not injected into transcript.
    last_note: Option<String>,
    usage: UsageMeter,
}

impl TurnDriver {
    /// Build from process env (`PHI_*`). Config lives in the product binary.
    #[must_use]
    pub fn from_env() -> Self {
        let context_window = Self::load_context_window_from_env();
        match Self::load_llm_config_from_env() {
            Ok(cfg) => {
                let model_id = cfg.model.clone();
                let api_style = cfg.api_style.as_ref().to_owned();
                let api_base = cfg.api_base.clone();
                // Product strategy: lean chat context (not owned by phi-ext-llm).
                let agent = Arc::new(
                    OpenAiCompatRuntime::new(cfg).with_projector(Arc::new(ChatTextOnly)),
                );
                Self {
                    host: Some(SessionHost::new(agent)),
                    config_error: None,
                    model_id,
                    api_style,
                    api_base,
                    context_window,
                    last_note: None,
                    usage: UsageMeter::default(),
                }
            }
            Err(e) => Self {
                host: None,
                config_error: Some(e),
                model_id: "unconfigured".into(),
                api_style: "—".into(),
                api_base: "—".into(),
                context_window,
                last_note: None,
                usage: UsageMeter::default(),
            },
        }
    }

    #[must_use]
    pub fn channel(&self) -> ChannelInfo {
        ChannelInfo {
            model_id: self.model_id.clone(),
            api_style: self.api_style.clone(),
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

    /// Product-owned env mapping (not in `phi-ext-llm`).
    fn load_llm_config_from_env() -> Result<LlmConfig, String> {
        let api_key = Self::env_trim("PHI_API_KEY");
        if api_key.is_empty() {
            return Err("set PHI_API_KEY to call a real model".into());
        }
        let api_base = Self::env_trim("PHI_API_BASE");
        let api_base = if api_base.is_empty() {
            "https://api.openai.com/v1".into()
        } else {
            api_base.trim_end_matches('/').to_owned()
        };
        let model = Self::env_trim("PHI_MODEL");
        let model = if model.is_empty() {
            "gpt-4o-mini".into()
        } else {
            model
        };
        let style_raw = Self::env_trim("PHI_API_STYLE");
        let api_style = if style_raw.is_empty() {
            ApiStyle::default()
        } else {
            style_raw.parse::<ApiStyle>().map_err(|_| {
                format!(
                    "invalid PHI_API_STYLE `{style_raw}` (use `responses` or `completions`)"
                )
            })?
        };
        Ok(LlmConfig {
            api_base,
            api_key,
            model,
            api_style,
        })
    }

    /// Context window denominator (`PHI_CONTEXT_WINDOW`, default 128000).
    fn load_context_window_from_env() -> u64 {
        let raw = Self::env_trim("PHI_CONTEXT_WINDOW");
        if raw.is_empty() {
            return 128_000;
        }
        raw.parse::<u64>().unwrap_or(128_000).max(1)
    }

    fn env_trim(key: &str) -> String {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_default()
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

// ── Product history strategy (phi-code owns this policy) ───────────────────

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
