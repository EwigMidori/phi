//! Agent shell: **route only** — tick / draw / handle among owned collaborators.
//!
//! # Forbidden (read before editing)
//!
//! **Do not casually add methods or fields to [`AgentShell`].**
//!
//! - Status chrome → [`crate::status_bar::StatusBar`]
//! - History / selection → [`crate::scrollback_pane::ScrollbackPane`]
//! - Prompt / paste → [`crate::prompt_pane::PromptPane`]
//! - Turns / LLM → [`crate::turn_driver::TurnDriver`]
//!
//! If you are about to write `fn paint_*` or `fn handle_*_detail` here, **stop**
//! and put a message on the responsible object instead. Shell is a thin
//! coordinator, not a dumping ground (Evans boundary / Kay objects / Ousterhout
//! deep modules). Violating this will be rejected in review.

use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use phi_code_ui::SystemClipboard;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use xai_ratatui_textarea::ClipboardProvider;

use crate::prompt_pane::PromptPane;
use crate::scrollback_pane::ScrollbackPane;
use crate::status_bar::{classify_note, NoteKind, StatusBar, StatusSnapshot, StreamKind};
use crate::turn_driver::{SubmitOutcome, TurnDriver};

/// Second Ctrl+C must arrive within this window to quit.
const QUIT_CTRL_C_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Prompt,
    Scrollback,
}

/// Thin coordinator. See module docs — **do not grow this type**.
pub struct AgentShell {
    focus: Focus,
    prompt: PromptPane,
    scrollback: ScrollbackPane,
    driver: TurnDriver,
    status: StatusBar,
    quit: bool,
    ctrl_c_armed_at: Option<Instant>,
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
            prompt,
            scrollback: ScrollbackPane::new(),
            driver,
            status,
            quit: false,
            ctrl_c_armed_at: None,
        }
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Route a transient note to the status bar (typed by classifier).
    pub fn notify(&mut self, note: impl Into<String>) {
        let text = note.into();
        let kind = classify_note(&text);
        self.status.set_note(kind, text);
    }

    pub fn tick(&mut self) {
        self.expire_ctrl_c_arm();
        if self.driver.tick(self.scrollback.scrollback_mut()) {
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
        StatusSnapshot {
            stream,
            model_id: self.driver.model_id().to_owned(),
            usage_bar: self.driver.usage_bar(),
            multiline: self.prompt.is_multiline(),
            channel_lines: self.driver.channel_detail_lines(),
            usage_lines: self.driver.usage_detail_lines(),
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
                self.ctrl_c_armed_at = None;
                if self.scrollback.has_selection() {
                    self.scrollback.clear_selection();
                    self.status.clear_note();
                }
            }
            KeyCode::Tab => {
                self.ctrl_c_armed_at = None;
                self.focus = match self.focus {
                    Focus::Prompt => Focus::Scrollback,
                    Focus::Scrollback => Focus::Prompt,
                };
            }
            KeyCode::Char('m') if ctrl => {
                self.ctrl_c_armed_at = None;
                self.prompt.toggle_multiline();
            }
            KeyCode::Char('v' | 'V') if ctrl && matches!(self.focus, Focus::Prompt) => {
                self.ctrl_c_armed_at = None;
                let mut clip = SystemClipboard::new();
                if let Some(text) = ClipboardProvider::get(&mut clip) {
                    self.prompt.apply_paste(&text);
                    self.notify("pasted");
                }
            }
            KeyCode::Enter if matches!(self.focus, Focus::Prompt) => {
                self.ctrl_c_armed_at = None;
                if shift || alt {
                    self.prompt.insert_newline();
                } else {
                    let msg = self.prompt.submit_candidate();
                    match self
                        .driver
                        .submit(self.scrollback.scrollback_mut(), &msg)
                    {
                        SubmitOutcome::Accepted => {
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
            }
            _ if matches!(self.focus, Focus::Scrollback) => {
                self.ctrl_c_armed_at = None;
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
                self.ctrl_c_armed_at = None;
                self.prompt.input_key(key);
            }
            _ => {}
        }
    }

    fn handle_ctrl_c(&mut self) {
        if self.scrollback.has_selection() {
            self.ctrl_c_armed_at = None;
            if let Some(n) = self.scrollback.copy_selection() {
                self.notify(format!("copied selection ({n} chars)"));
            }
            return;
        }

        if !self.prompt.is_empty() {
            self.ctrl_c_armed_at = None;
            let text = self.prompt.text();
            let n = text.chars().count();
            let mut clip = SystemClipboard::new();
            clip.copy_text(&text);
            self.prompt.clear();
            self.notify(format!("copied prompt ({n} chars) · cleared"));
            return;
        }

        let now = Instant::now();
        if let Some(armed_at) = self.ctrl_c_armed_at {
            if now.duration_since(armed_at) <= QUIT_CTRL_C_WINDOW {
                self.quit = true;
                return;
            }
        }
        self.ctrl_c_armed_at = Some(now);
        self.notify("press Ctrl+C again to quit");
    }

    fn expire_ctrl_c_arm(&mut self) {
        let Some(armed_at) = self.ctrl_c_armed_at else {
            return;
        };
        if Instant::now().duration_since(armed_at) <= QUIT_CTRL_C_WINDOW {
            return;
        }
        self.ctrl_c_armed_at = None;
        // Clear only the quit reminder, not other notes.
        if self
            .status
            .note()
            .is_some_and(|n| n.text == "press Ctrl+C again to quit")
        {
            self.status.clear_note();
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
