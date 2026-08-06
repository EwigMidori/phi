//! Turn driver: submit user text → [`SessionTurnRunner`] → [`AgentEvent`] → Scrollback.

use std::sync::Arc;

use phi_code_core::{AgentEvent, LlmConfig, OpenAiCompatRuntime, SessionTurnRunner};
use phi_code_ui::Scrollback;

/// Bridges product turn runner to the scrollback view.
pub struct TurnDriver {
    runner: Option<SessionTurnRunner>,
    /// Startup config error (e.g. missing PHI_API_KEY); shown once / on submit.
    config_error: Option<String>,
    /// Model label for status chrome.
    model_label: String,
    /// True while the latest turn is still receiving reasoning (CoT) deltas.
    thinking: bool,
}

impl TurnDriver {
    /// Build from env (`PHI_API_KEY` / `PHI_API_BASE` / `PHI_MODEL`).
    #[must_use]
    pub fn from_env() -> Self {
        match LlmConfig::from_env() {
            Ok(cfg) => {
                let model_label =
                    format!("{} ({}) @ {}", cfg.model, cfg.api_style.as_ref(), cfg.api_base);
                let agent = Arc::new(OpenAiCompatRuntime::new(cfg));
                Self {
                    runner: Some(SessionTurnRunner::new(agent)),
                    config_error: None,
                    model_label,
                    thinking: false,
                }
            }
            Err(e) => Self {
                runner: None,
                config_error: Some(e.to_string()),
                model_label: "unconfigured".into(),
                thinking: false,
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

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.runner.as_ref().is_some_and(SessionTurnRunner::is_busy)
    }

    /// True while CoT/reasoning is arriving (answer body may still be empty).
    #[must_use]
    pub fn is_thinking(&self) -> bool {
        self.thinking
    }

    /// Start a turn: push user + begin assistant stream + spawn LLM job.
    ///
    /// Returns `false` when busy or empty. Config/API start failures still return
    /// `true` after writing an error into the assistant stream and finishing it.
    pub fn submit(&mut self, scrollback: &mut Scrollback, user: &str) -> bool {
        if self.is_busy() || scrollback.is_streaming() {
            return false;
        }
        let msg = user.trim();
        if msg.is_empty() {
            return false;
        }

        scrollback.push_user(msg);
        scrollback.begin_assistant_stream();
        scrollback.scroll_to_bottom();
        self.thinking = false;

        if let Some(err) = &self.config_error {
            let err = err.clone();
            scrollback.append_assistant_delta(&format!(
                "**Configuration error**\n\n{err}\n\nSet `PHI_API_KEY` / `PHI_API_BASE` / `PHI_MODEL` / `PHI_API_STYLE`.\n"
            ));
            scrollback.finish_assistant_stream();
            return true;
        }

        let Some(runner) = self.runner.as_mut() else {
            scrollback.append_assistant_delta("**No LLM runtime configured.**\n");
            scrollback.finish_assistant_stream();
            return true;
        };

        let history = scrollback.items().to_vec();
        if let Err(e) = runner.start_turn(history) {
            scrollback.append_assistant_delta(&format!("\n\n**Failed to start turn:** {e}\n"));
            scrollback.finish_assistant_stream();
            self.thinking = false;
        }
        true
    }

    /// Drain kernel [`AgentEvent`]s into scrollback.
    ///
    /// Returns `true` when a stream finished on this tick (clear painter).
    /// Tool / approval events are ignored until product hosts them (channel already carries them).
    pub fn tick(&mut self, scrollback: &mut Scrollback) -> bool {
        let Some(runner) = self.runner.as_mut() else {
            return false;
        };
        let events = runner.poll();
        if events.is_empty() {
            return false;
        }
        let mut finished = false;
        for ev in events {
            match ev {
                AgentEvent::TextDelta { text } => {
                    if text.is_empty() {
                        continue;
                    }
                    self.thinking = false;
                    scrollback.append_assistant_delta(&text);
                    scrollback.scroll_to_bottom();
                }
                AgentEvent::ReasoningDelta { text } => {
                    if text.is_empty() {
                        continue;
                    }
                    self.thinking = true;
                    scrollback.append_reasoning_delta(&text);
                    scrollback.scroll_to_bottom();
                }
                AgentEvent::Error { message } => {
                    self.thinking = false;
                    scrollback.append_assistant_delta(&format!("\n\n**Error:** {message}\n"));
                    scrollback.scroll_to_bottom();
                }
                AgentEvent::Finished { .. } => {
                    self.thinking = false;
                    scrollback.finish_assistant_stream();
                    scrollback.scroll_to_bottom();
                    finished = true;
                }
                AgentEvent::ToolCall { .. }
                | AgentEvent::ToolResult { .. }
                | AgentEvent::ToolApprovalRequired { .. }
                | AgentEvent::Unknown { .. } => {
                    // Forwarded by runner; UI host for tools/approvals lands later.
                }
            }
        }
        finished
    }
}
