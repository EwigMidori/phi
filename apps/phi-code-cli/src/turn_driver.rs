//! Turn driver: submit user text → [`SessionTurnRunner`] → stream into Scrollback.

use std::sync::Arc;

use phi_code_core::{
    LlmConfig, OpenAiCompatRuntime, SessionTurnRunner, TurnProgress,
};
use phi_code_ui::Scrollback;

/// Bridges product turn runner to the scrollback view.
pub struct TurnDriver {
    runner: Option<SessionTurnRunner>,
    /// Startup config error (e.g. missing PHI_API_KEY); shown once / on submit.
    config_error: Option<String>,
    /// Model label for status chrome.
    model_label: String,
}

impl TurnDriver {
    /// Build from env (`PHI_API_KEY` / `PHI_API_BASE` / `PHI_MODEL`).
    #[must_use]
    pub fn from_env() -> Self {
        match LlmConfig::from_env() {
            Ok(cfg) => {
                let style = match cfg.api_style {
                    phi_code_core::ApiStyle::Responses => "responses",
                    phi_code_core::ApiStyle::Completions => "completions",
                };
                let model_label = format!("{} ({style}) @ {}", cfg.model, cfg.api_base);
                let agent = Arc::new(OpenAiCompatRuntime::new(cfg));
                Self {
                    runner: Some(SessionTurnRunner::new(agent)),
                    config_error: None,
                    model_label,
                }
            }
            Err(e) => Self {
                runner: None,
                config_error: Some(e.to_string()),
                model_label: "unconfigured".into(),
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

        if let Some(err) = &self.config_error {
            let err = err.clone();
            scrollback.append_assistant_delta(&format!(
                "**Configuration error**\n\n{err}\n\nSet `AGENT_LLM__API_KEY` / `BASE_URL` / `MODEL` / `API_STYLE` (or PHI_* fallbacks).\n"
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
        }
        true
    }

    /// Drain runner progress into scrollback.
    ///
    /// Returns `true` when a stream finished on this tick (clear painter).
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
                TurnProgress::TextDelta(text) => {
                    scrollback.append_assistant_delta(&text);
                    scrollback.scroll_to_bottom();
                }
                TurnProgress::Error(message) => {
                    scrollback.append_assistant_delta(&format!("\n\n**Error:** {message}\n"));
                    scrollback.scroll_to_bottom();
                }
                TurnProgress::Finished => {
                    scrollback.finish_assistant_stream();
                    scrollback.scroll_to_bottom();
                    finished = true;
                }
            }
        }
        finished
    }
}
