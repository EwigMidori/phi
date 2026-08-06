//! Turn driver: CLI composition — env → `phi-ext-llm` runtime → [`SessionHost`] → view.

use std::sync::Arc;

use phi_code_core::{KernelEvent, SessionHost};
use phi_code_ui::Scrollback;
use phi_ext_llm::{ApiStyle, LlmConfig, OpenAiCompatRuntime};

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

/// Bridges kernel session host to the scrollback view.
pub struct TurnDriver {
    host: Option<SessionHost>,
    config_error: Option<String>,
    model_label: String,
    /// Last status (errors, approval, pump) — not injected into transcript.
    last_note: Option<String>,
}

impl TurnDriver {
    /// Build from process env (`PHI_*`). Config lives in the product binary.
    #[must_use]
    pub fn from_env() -> Self {
        match load_llm_config_from_env() {
            Ok(cfg) => {
                let model_label =
                    format!("{} ({}) @ {}", cfg.model, cfg.api_style.as_ref(), cfg.api_base);
                let agent = Arc::new(OpenAiCompatRuntime::new(cfg));
                Self {
                    host: Some(SessionHost::new(agent)),
                    config_error: None,
                    model_label,
                    last_note: None,
                }
            }
            Err(e) => Self {
                host: None,
                config_error: Some(e),
                model_label: "unconfigured".into(),
                last_note: None,
            },
        }
    }

    #[must_use]
    pub fn model_label(&self) -> &str {
        &self.model_label
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

fn env_trim(key: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_default()
}
