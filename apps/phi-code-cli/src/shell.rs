//! Agent shell: routes events among prompt, scrollback, and turn driver.

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
use crate::turn_driver::TurnDriver;

/// Second Ctrl+C must arrive within this window to quit.
const QUIT_CTRL_C_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Prompt,
    Scrollback,
}

/// Orchestrates panes + turn driver. Host only calls tick / draw / handle.
pub struct AgentShell {
    focus: Focus,
    status_note: String,
    prompt: PromptPane,
    scrollback: ScrollbackPane,
    driver: TurnDriver,
    quit: bool,
    /// Instant of the first Ctrl+C in a double-tap quit sequence.
    ctrl_c_armed_at: Option<Instant>,
}

impl AgentShell {
    #[must_use]
    pub fn new() -> Self {
        // TextArea gets its own clipboard provider; pane also holds one for selection copy.
        let prompt = PromptPane::new(Box::new(SystemClipboard::new()));
        let driver = TurnDriver::from_env();
        let status_note = driver
            .config_error()
            .map(|e| format!("warn: {e}"))
            .unwrap_or_default();
        Self {
            focus: Focus::Prompt,
            status_note,
            prompt,
            scrollback: ScrollbackPane::new(),
            driver,
            quit: false,
            ctrl_c_armed_at: None,
        }
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn notify(&mut self, note: impl Into<String>) {
        self.status_note = note.into();
    }

    pub fn tick(&mut self) {
        self.expire_ctrl_c_arm();
        if self.driver.tick(self.scrollback.scrollback_mut()) {
            self.scrollback.clear_stream_renderer();
        }
        self.scrollback.tick();
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let prompt_h = self.prompt.desired_height();
        let [status, sb_rect, prompt, shortcuts] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(prompt_h),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let mode = if self.prompt.is_multiline() {
            "multiline"
        } else {
            "single"
        };
        let stream = if self.driver.is_thinking() {
            "thinking"
        } else if self.driver.is_busy() || self.scrollback.scrollback().is_streaming() {
            "streaming"
        } else {
            "idle"
        };
        let focus_label = match self.focus {
            Focus::Prompt => "prompt",
            Focus::Scrollback => "scrollback",
        };
        let note = if self.status_note.is_empty() {
            String::new()
        } else {
            format!("  {}", self.status_note)
        };
        frame.render_widget(
            Paragraph::new(format!(
                " focus:{focus_label}  {mode}  {stream}  model:{}  turns:{}  h:{}{note} ",
                self.driver.model_label(),
                self.scrollback.scrollback().items().len(),
                self.scrollback.scrollback().total_height()
            )),
            status,
        );

        let sb_focused = matches!(self.focus, Focus::Scrollback);
        self.scrollback.paint(frame, sb_rect, sb_focused);
        self.prompt
            .paint(frame, prompt, matches!(self.focus, Focus::Prompt));

        frame.render_widget(
            Paragraph::new(
                " select · y copy · C-c clear prompt · C-c C-c quit (empty) · C-v paste ",
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

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
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
            self.focus = Focus::Scrollback;
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
            // Prompt non-empty → copy+clear (no quit). Empty → double Ctrl+C quit.
            KeyCode::Char('c' | 'C') if ctrl => {
                self.handle_ctrl_c();
            }
            // Esc only clears selection — never quits.
            KeyCode::Esc => {
                if self.scrollback.has_selection() {
                    self.scrollback.clear_selection();
                    self.status_note.clear();
                }
                self.ctrl_c_armed_at = None;
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
                    if self
                        .driver
                        .submit(self.scrollback.scrollback_mut(), &msg)
                    {
                        self.prompt.clear();
                        self.scrollback.clear_stream_renderer();
                        self.scrollback.clear_selection();
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
        // Non-empty prompt: copy + clear. Does not arm quit.
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

        // Empty prompt: double Ctrl+C within 500ms quits.
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

    /// Drop an expired first Ctrl+C so the status reminder does not linger.
    fn expire_ctrl_c_arm(&mut self) {
        let Some(armed_at) = self.ctrl_c_armed_at else {
            return;
        };
        if Instant::now().duration_since(armed_at) <= QUIT_CTRL_C_WINDOW {
            return;
        }
        self.ctrl_c_armed_at = None;
        if self.status_note == "press Ctrl+C again to quit" {
            self.status_note.clear();
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
