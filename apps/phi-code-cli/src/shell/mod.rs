//! # `shell` — terminal chrome only (View + input routing + model→view projection)
//!
//! Layering for this binary (MVVM-ish, not a framework):
//!
//! | Layer | Where | Responsibility |
//! |-------|--------|----------------|
//! | **Model / application** | [`crate::turn_driver`] | LLM session, submit/tick, usage *counts*, product policy |
//! | **View + chrome** | **this package** | layout, paint, focus, panes, status *formatting*, event→scrollback projection |
//! | **Host** | [`crate`] `main` | terminal lifecycle, event poll loop |
//!
//! [`AgentShell`] is the thin **ViewModel/coordinator**: it holds UI objects and a
//! `TurnDriver` handle, routes terminal events, projects model outputs into the
//! view, and builds paint-time snapshots. It must not own product policy.
//!
//! # Allowed in `shell/`
//!
//! - Ratatui / crossterm layout and paint
//! - Focus routing, mouse hit tests, keyboard→pane dispatch
//! - Prompt / scrollback / status **presentation** objects
//! - Host chrome protocols (e.g. double-Ctrl+C quit arming)
//! - Mapping [`crate::turn_driver::TickResult`] / history onto scrollback via
//!   [`scrollback_pane::ScrollbackPane`] methods (not free functions with side effects)
//! - Formatting raw [`crate::turn_driver::ChannelInfo`] / [`UsageInfo`] into status
//!   strings via **pure** helpers in [`status_format`]
//!
//! # Forbidden in `shell/` (reject in review)
//!
//! **Do not put application / domain logic in this directory.** Move it to
//! [`crate::turn_driver`] (or further into `phi-code-core` / product crates).
//!
//! Specifically **do not**:
//!
//! - Call or configure LLM runtimes, `SessionHost`, `phi-ext-llm`, API keys/env product policy
//! - Implement history projectors, transcript policies, or tool-approval product rules
//! - Aggregate token meters (session Σ); only **display** counts the model already computed
//! - Decide submit accept/reject product rules beyond pure view gates (e.g. already streaming)
//! - Add files under `shell/` for “just a little” product composition
//! - **Free functions with read/write side effects** (mutate panes, touch env, I/O).
//!   Side-effecting behavior is a method on the object that owns the state.
//!   Free functions may only be pure (args → value).
//!
//! # Forbidden in `turn_driver` (mirror rule)
//!
//! Application code must **not** import `phi_code_ui`, ratatui, or format status chrome.
//! If it mutates scrollback or paints, it belongs here instead.
//!
//! **Do not casually add methods or fields to [`AgentShell`].** Prefer a message
//! on the responsible object:
//!
//! - Status chrome → [`status_bar::StatusBar`] / [`status_format`]
//! - History / selection / event→view → [`scrollback_pane::ScrollbackPane`]
//! - Prompt / paste → [`prompt_pane::PromptPane`]
//! - Double-Ctrl+C quit → [`quit_protocol::QuitProtocol`]
//! - Turns / LLM / usage counts / config → [`crate::turn_driver::TurnDriver`] (**outside** this package)
//!
//! If you are about to write product policy or env loading here — **stop**.

mod prompt_pane;
mod quit_protocol;
mod scrollback_pane;
mod status_bar;
mod status_format;

use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use phi_code_ui::SystemClipboard;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use xai_ratatui_textarea::ClipboardProvider;

use crate::turn_driver::{SubmitOutcome, TurnDriver};

use prompt_pane::PromptPane;
use quit_protocol::{IdleCtrlC, QuitProtocol, QUIT_REMINDER};
use scrollback_pane::ScrollbackPane;
use status_bar::{classify_note, NoteKind, StatusBar, StatusSnapshot, StreamKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Prompt,
    Scrollback,
}

/// Thin UI coordinator. See module docs — **do not grow this type with product logic**.
pub struct AgentShell {
    focus: Focus,
    quit: QuitProtocol,
    prompt: PromptPane,
    scrollback: ScrollbackPane,
    /// Application model — lives in [`crate::turn_driver`], only composed here.
    driver: TurnDriver,
    status: StatusBar,
}

impl AgentShell {
    #[must_use]
    pub fn new() -> Self {
        let prompt = PromptPane::new(Box::new(SystemClipboard::new()));
        let driver = TurnDriver::from_env();
        let mut status = StatusBar::new();
        if let Some(err) = driver.config_error() {
            status.set_note(NoteKind::Warn, format!("warn: {err}"));
        }
        Self {
            focus: Focus::Prompt,
            quit: QuitProtocol::new(),
            prompt,
            scrollback: ScrollbackPane::new(),
            driver,
            status,
        }
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit.should_quit()
    }

    /// Route a transient note to the status bar (typed by classifier).
    pub fn notify(&mut self, note: impl Into<String>) {
        let text = note.into();
        let kind = classify_note(&text);
        self.status.set_note(kind, text);
    }

    pub fn tick(&mut self) {
        if self.quit.expire()
            && self
                .status
                .note()
                .is_some_and(|n| n.text == QUIT_REMINDER)
        {
            self.status.clear_note();
        }
        let tick = self.driver.tick();
        if self.scrollback.apply_tick(&self.driver, &tick) {
            self.scrollback.clear_stream_renderer();
        }
        if let Some(note) = self.driver.take_last_note() {
            self.notify(note);
        }
        self.scrollback.tick();
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let prompt_h = self.prompt.desired_height();
        let status_h = self.status.height();
        let [status_area, sb_rect, prompt, shortcuts] = Layout::vertical([
            Constraint::Length(status_h),
            Constraint::Min(1),
            Constraint::Length(prompt_h),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let snapshot = self.status_snapshot();
        self.status.paint(frame, status_area, &snapshot);

        let sb_focused = matches!(self.focus, Focus::Scrollback);
        self.scrollback.paint(frame, sb_rect, sb_focused);
        self.prompt
            .paint(frame, prompt, matches!(self.focus, Focus::Prompt));

        frame.render_widget(
            Paragraph::new(
                " click status for details · select · C-c copy · y copy · C-c C-c quit · C-v paste ",
            )
            .style(Style::default().fg(Color::DarkGray)),
            shortcuts,
        );
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Paste(payload) if matches!(self.focus, Focus::Prompt) => {
                self.prompt.apply_paste(&payload);
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            _ => {}
        }
    }

    fn status_snapshot(&self) -> StatusSnapshot {
        let stream = if self.scrollback.scrollback().is_thinking() {
            StreamKind::Thinking
        } else if self.driver.is_busy() || self.scrollback.scrollback().is_streaming() {
            StreamKind::Streaming
        } else {
            StreamKind::Idle
        };
        let sb = self.scrollback.scrollback();
        let channel = self.driver.channel();
        let usage = self.driver.usage();
        StatusSnapshot {
            stream,
            model_id: channel.model_id.clone(),
            usage_bar: status_format::usage_bar(&usage),
            multiline: self.prompt.is_multiline(),
            channel_lines: status_format::channel_detail_lines(&channel),
            usage_lines: status_format::usage_detail_lines(&usage),
            layout_line: format!(
                "  layout:  {} entries · {} rows",
                sb.items().len(),
                sb.total_height()
            ),
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.status.contains(mouse.column, mouse.row)
        {
            self.status.toggle();
            return;
        }

        let prompt_hit = self.prompt.hit();
        if rect_contains(prompt_hit, mouse.column, mouse.row) {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.focus = Focus::Prompt;
                self.scrollback.clear_selection();
                self.scrollback.end_scrollbar_drag();
            }
            if matches!(self.focus, Focus::Prompt) {
                self.prompt.handle_mouse(mouse);
            }
            return;
        }

        let mut note = None;
        let mut notify = |s: String| {
            note = Some(s);
        };
        if self.scrollback.on_mouse(mouse, &mut notify) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left) => {
                    self.focus = Focus::Scrollback;
                }
                _ => {}
            }
            if let Some(n) = note {
                self.notify(n);
            }
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('c' | 'C') if ctrl => {
                self.handle_ctrl_c();
            }
            KeyCode::Esc => {
                self.quit.disarm();
                if self.scrollback.has_selection() {
                    self.scrollback.clear_selection();
                    self.status.clear_note();
                }
            }
            KeyCode::Tab => {
                self.quit.disarm();
                self.focus = match self.focus {
                    Focus::Prompt => Focus::Scrollback,
                    Focus::Scrollback => Focus::Prompt,
                };
            }
            KeyCode::Char('m') if ctrl => {
                self.quit.disarm();
                self.prompt.toggle_multiline();
            }
            KeyCode::Char('v' | 'V') if ctrl && matches!(self.focus, Focus::Prompt) => {
                self.quit.disarm();
                let mut clip = SystemClipboard::new();
                if let Some(text) = ClipboardProvider::get(&mut clip) {
                    self.prompt.apply_paste(&text);
                    self.notify("pasted");
                }
            }
            KeyCode::Enter if matches!(self.focus, Focus::Prompt) => {
                self.quit.disarm();
                if shift || alt {
                    self.prompt.insert_newline();
                } else {
                    self.submit_prompt();
                }
            }
            _ if matches!(self.focus, Focus::Scrollback) => {
                self.quit.disarm();
                let mut note = None;
                let mut notify = |s: String| {
                    note = Some(s);
                };
                if self.scrollback.on_key(key, &mut notify) {
                    if let Some(n) = note {
                        self.notify(n);
                    }
                }
            }
            _ if matches!(self.focus, Focus::Prompt) => {
                self.quit.disarm();
                self.prompt.input_key(key);
            }
            _ => {}
        }
    }

    /// View gate (streaming) + application submit + scrollback projection.
    fn submit_prompt(&mut self) {
        // View-only gate: do not start another turn while live overlay is open.
        if self.scrollback.scrollback().is_streaming() {
            return;
        }
        let msg = self.prompt.submit_candidate();
        match self.driver.submit(&msg) {
            SubmitOutcome::Accepted => {
                self.scrollback.apply_submit_accepted(&self.driver);
                self.prompt.clear();
                self.scrollback.clear_stream_renderer();
                self.scrollback.clear_selection();
            }
            SubmitOutcome::Rejected | SubmitOutcome::Failed => {}
        }
        if let Some(n) = self.driver.take_last_note() {
            self.notify(n);
        }
    }

    fn handle_ctrl_c(&mut self) {
        if self.scrollback.has_selection() {
            self.quit.disarm();
            if let Some(n) = self.scrollback.copy_selection() {
                self.notify(format!("copied selection ({n} chars)"));
            }
            return;
        }

        if !self.prompt.is_empty() {
            self.quit.disarm();
            let text = self.prompt.text();
            let n = text.chars().count();
            let mut clip = SystemClipboard::new();
            clip.copy_text(&text);
            self.prompt.clear();
            self.notify(format!("copied prompt ({n} chars) · cleared"));
            return;
        }

        match self.quit.on_idle_ctrl_c() {
            IdleCtrlC::ConfirmedQuit => {}
            IdleCtrlC::Armed => self.notify(QUIT_REMINDER),
        }
    }
}

impl Default for AgentShell {
    fn default() -> Self {
        Self::new()
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}
