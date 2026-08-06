//! Turn driver: CLI composition — env → `phi-ext-llm` runtime → [`SessionHost`] → view.

use std::sync::Arc;

use phi_code_core::{KernelEvent, SessionHost, TurnItem, Usage};
use phi_code_ui::Scrollback;
use phi_ext_llm::{ApiStyle, HistoryProjector, LlmConfig, OpenAiCompatRuntime};

/// Result of [`TurnDriver::submit`] — shell clears the prompt only on [`Accepted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// User recorded + job enqueued; prompt may be cleared.
    Accepted,
    /// Busy / still streaming / empty input — keep prompt text.
    Rejected,
    /// Config or host failure — keep prompt; note via [`TurnDriver::take_last_note`].
    Failed,
}

/// Product object: last job + session totals from provider [`Usage`] only.
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

    /// Context filled on the last request (provider `prompt_tokens`, else `total_tokens`).
    fn context_used(&self) -> Option<u64> {
        let u = self.last.as_ref()?;
        u.prompt_tokens.or(u.total_tokens)
    }

    /// Compact bar: `used / window` humanized, e.g. `12K / 128K` or `— / 1M`.
    fn bar_fragment(&self, context_window: u64) -> String {
        let window = format_token_qty(context_window);
        match self.context_used() {
            Some(used) => format!("{} / {}", format_token_qty(used), window),
            None => format!("— / {window}"),
        }
    }

    /// Extra lines for the expanded status panel.
    fn detail_lines(&self, context_window: u64) -> Vec<String> {
        let mut lines = Vec::new();
        match self.context_used() {
            Some(used) => lines.push(format!(
                "  context: {} / {} ({} / {} tokens)",
                format_token_qty(used),
                format_token_qty(context_window),
                used,
                context_window
            )),
            None => lines.push(format!(
                "  context: — / {} (window {}; waiting for provider usage)",
                format_token_qty(context_window),
                context_window
            )),
        }
        if let Some(u) = &self.last {
            let mut last = Vec::new();
            if let Some(p) = u.prompt_tokens {
                last.push(format!("prompt {p}"));
            }
            if let Some(c) = u.completion_tokens {
                last.push(format!("completion {c}"));
            }
            if let Some(t) = u.total_tokens {
                last.push(format!("total {t}"));
            }
            if !last.is_empty() {
                lines.push(format!("  last turn: {}", last.join(" · ")));
            }
        }
        let mut sigma = Vec::new();
        if self.session_prompt > 0 {
            sigma.push(format!("prompt {}", self.session_prompt));
        }
        if self.session_completion > 0 {
            sigma.push(format!("completion {}", self.session_completion));
        }
        if self.session_total > 0 {
            sigma.push(format!("total {}", self.session_total));
        }
        if !sigma.is_empty() {
            lines.push(format!("  session Σ: {}", sigma.join(" · ")));
        }
        lines
    }
}

/// Human quantity for the bar: `999`, `12K`, `1M`, `1.5M` (provider counts only).
fn format_token_qty(n: u64) -> String {
    if n >= 1_000_000 {
        let whole = n / 1_000_000;
        let tenths = (n % 1_000_000) / 100_000;
        if tenths == 0 {
            format!("{whole}M")
        } else {
            format!("{whole}.{tenths}M")
        }
    } else if n >= 1000 {
        // Round to nearest K for scannability.
        let k = n.div_ceil(1000);
        format!("{k}K")
    } else {
        n.to_string()
    }
}

/// Bridges kernel session host to the scrollback view.
pub struct TurnDriver {
    host: Option<SessionHost>,
    config_error: Option<String>,
    /// Short model id for the collapsed status bar.
    model_id: String,
    api_style: String,
    api_base: String,
    /// Model context window (product config); denominator of the usage bar.
    context_window: u64,
    /// Last status (errors, approval, pump) — not injected into transcript.
    last_note: Option<String>,
    usage: UsageMeter,
}

impl TurnDriver {
    /// Build from process env (`PHI_*`). Config lives in the product binary.
    #[must_use]
    pub fn from_env() -> Self {
        let context_window = load_context_window_from_env();
        match load_llm_config_from_env() {
            Ok(cfg) => {
                let model_id = cfg.model.clone();
                let api_style = cfg.api_style.as_ref().to_owned();
                let api_base = cfg.api_base.clone();
                // Product strategy: lean chat context (not owned by phi-ext-llm).
                let agent = Arc::new(
                    OpenAiCompatRuntime::new(cfg)
                        .with_projector(Arc::new(ChatTextOnly)),
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

    /// Context bar: `used / window` (e.g. `12K / 128K`). Always shows the window.
    #[must_use]
    pub fn usage_bar(&self) -> String {
        self.usage.bar_fragment(self.context_window)
    }

    /// Short model name for the collapsed bar.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Full channel summary for the expanded panel.
    #[must_use]
    pub fn channel_detail_lines(&self) -> Vec<String> {
        vec![
            format!("  model:  {}", self.model_id),
            format!("  style:  {}", self.api_style),
            format!("  base:   {}", self.api_base),
            format!(
                "  window: {} ({} tokens)",
                format_token_qty(self.context_window),
                self.context_window
            ),
        ]
    }

    /// Token detail lines for the expanded panel.
    #[must_use]
    pub fn usage_detail_lines(&self) -> Vec<String> {
        self.usage.detail_lines(self.context_window)
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

    /// Submit user text via kernel transcript + SendQueue.
    pub fn submit(&mut self, scrollback: &mut Scrollback, user: &str) -> SubmitOutcome {
        if self.is_busy() || scrollback.is_streaming() {
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
            Ok(()) => match host.history() {
                Ok(items) => {
                    scrollback.set_durable(items);
                    scrollback.begin_live();
                    self.last_note = None;
                    SubmitOutcome::Accepted
                }
                Err(e) => {
                    self.last_note = Some(format!("history: {e}"));
                    // User is already recorded; still start live for events.
                    scrollback.begin_live();
                    SubmitOutcome::Accepted
                }
            },
            Err(e) => {
                self.last_note = Some(e);
                SubmitOutcome::Failed
            }
        }
    }

    /// Drain kernel bus (+ host pump errors) into the scrollback projection.
    ///
    /// Returns `true` when a generation finished this tick (clear painter stream).
    pub fn tick(&mut self, scrollback: &mut Scrollback) -> bool {
        let Some(host) = self.host.as_mut() else {
            return false;
        };
        let batch = host.poll_events();
        let mut finished = false;
        let had_pump_error = batch.pump_error.is_some();
        if let Some(err) = batch.pump_error {
            self.last_note = Some(err);
            scrollback.end_live();
            if let Ok(items) = host.history() {
                scrollback.set_durable(items);
            }
            finished = true;
        }
        if batch.lagged {
            // Dropped events — resync durable truth; clear live to avoid ghosts.
            if let Ok(items) = host.history() {
                scrollback.set_durable(items);
            }
            if !host.is_busy() {
                scrollback.end_live();
            }
            self.last_note = Some("event bus lagged — resynced from transcript".into());
        }
        if batch.events.is_empty() && !had_pump_error && !batch.lagged {
            return false;
        }
        for ev in batch.events {
            match ev {
                KernelEvent::GenerationStart { .. } => {
                    if !scrollback.is_streaming() {
                        scrollback.begin_live();
                    }
                    // Durable already has the user row from submit.
                    if let Ok(items) = host.history() {
                        scrollback.set_durable(items);
                    }
                }
                KernelEvent::GenerationReasoningDelta { text, .. } => {
                    // Live only — no full history reload per token.
                    scrollback.live_reasoning_delta(&text);
                }
                KernelEvent::GenerationTextDelta { text, .. } => {
                    // Live only; reload durable once when CoT span ends.
                    if scrollback.live_text_delta(&text) {
                        if let Ok(items) = host.history() {
                            scrollback.set_durable(items);
                        }
                    }
                }
                KernelEvent::GenerationToolCall { .. }
                | KernelEvent::GenerationToolResult { .. } => {
                    if let Ok(items) = host.history() {
                        scrollback.set_durable(items);
                    }
                }
                KernelEvent::GenerationToolApprovalRequired {
                    tool_name, input, ..
                } => {
                    self.last_note = Some(format!("approval required: {tool_name} {input}"));
                    if let Ok(items) = host.history() {
                        scrollback.set_durable(items);
                    }
                }
                KernelEvent::GenerationAgentUnknown { kind, .. } => {
                    self.last_note = Some(format!("agent unknown event: {kind}"));
                }
                KernelEvent::GenerationUsage { usage, .. } => {
                    self.usage.observe(usage);
                }
                KernelEvent::GenerationDone { .. } => {
                    if let Ok(items) = host.history() {
                        scrollback.set_durable(items);
                    }
                    scrollback.end_live();
                    finished = true;
                }
                KernelEvent::GenerationStopped { partial, .. } => {
                    if let Ok(items) = host.history() {
                        scrollback.set_durable(items);
                    }
                    scrollback.end_live();
                    if !partial.is_empty() {
                        self.last_note =
                            Some("stopped (partial discarded from durable write)".into());
                    }
                    finished = true;
                }
                KernelEvent::GenerationError { message, .. } => {
                    if let Ok(items) = host.history() {
                        scrollback.set_durable(items);
                    }
                    scrollback.end_live();
                    self.last_note = Some(format!("error: {message}"));
                    finished = true;
                }
            }
        }
        finished
    }
}

// ── Product history strategy (phi-code owns this policy) ───────────────────

/// Lean multi-turn context: user + non-empty assistant only.
///
/// Reasoning / tool rows stay in transcript for UI; they are not re-sent on this
/// product’s default chat path. Other hosts inject their own [`HistoryProjector`].
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

/// Product-owned env mapping (not in `phi-ext-llm`).
fn load_llm_config_from_env() -> Result<LlmConfig, String> {
    let api_key = env_trim("PHI_API_KEY");
    if api_key.is_empty() {
        return Err("set PHI_API_KEY to call a real model".into());
    }
    let api_base = env_trim("PHI_API_BASE");
    let api_base = if api_base.is_empty() {
        "https://api.openai.com/v1".into()
    } else {
        api_base.trim_end_matches('/').to_owned()
    };
    let model = env_trim("PHI_MODEL");
    let model = if model.is_empty() {
        "gpt-4o-mini".into()
    } else {
        model
    };
    let style_raw = env_trim("PHI_API_STYLE");
    let api_style = if style_raw.is_empty() {
        ApiStyle::default()
    } else {
        style_raw
            .parse::<ApiStyle>()
            .map_err(|_| {
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

/// Context window for the usage bar denominator (`PHI_CONTEXT_WINDOW`, default 128000).
fn load_context_window_from_env() -> u64 {
    let raw = env_trim("PHI_CONTEXT_WINDOW");
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
