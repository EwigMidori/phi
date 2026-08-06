//! Double-Ctrl+C quit protocol (shell host chrome — not product/domain).
//!
//! Owns only arm/quit bits. Shell still decides *when* idle Ctrl+C applies
//! (no selection, empty prompt); this object answers *what* happens next.
//! Do not fold LLM/session concerns into this type.

use std::time::{Duration, Instant};

/// Second Ctrl+C must arrive within this window to quit.
const QUIT_CTRL_C_WINDOW: Duration = Duration::from_millis(500);

/// Status note text while armed — shell/status match on this exact string.
pub const QUIT_REMINDER: &str = "press Ctrl+C again to quit";

/// Outcome of idle Ctrl+C (empty prompt, no selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleCtrlC {
    /// First press within a fresh window — shell should show [`QUIT_REMINDER`].
    Armed,
    /// Second press inside the window — host loop should exit.
    ConfirmedQuit,
}

/// Session-level quit arming. Not a product pane.
#[derive(Debug, Default)]
pub struct QuitProtocol {
    quit: bool,
    armed_at: Option<Instant>,
}

impl QuitProtocol {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Cancel a pending double-Ctrl+C (any other meaningful key/action).
    pub fn disarm(&mut self) {
        self.armed_at = None;
    }

    /// Idle Ctrl+C: first press arms, second within the window confirms quit.
    pub fn on_idle_ctrl_c(&mut self) -> IdleCtrlC {
        let now = Instant::now();
        if let Some(armed_at) = self.armed_at {
            if now.duration_since(armed_at) <= QUIT_CTRL_C_WINDOW {
                self.quit = true;
                return IdleCtrlC::ConfirmedQuit;
            }
        }
        self.armed_at = Some(now);
        IdleCtrlC::Armed
    }

    /// Drop a stale arm. Returns `true` when the quit-reminder note should clear.
    pub fn expire(&mut self) -> bool {
        let Some(armed_at) = self.armed_at else {
            return false;
        };
        if Instant::now().duration_since(armed_at) <= QUIT_CTRL_C_WINDOW {
            return false;
        }
        self.armed_at = None;
        true
    }
}
