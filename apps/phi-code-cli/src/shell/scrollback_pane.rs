//! Scrollback pane (shell UI): history layout, paint, selection, scrollbar, nav keys,
//! and model→view projection onto owned [`Scrollback`].
//!
//! Free functions here must stay pure (no read/write of process or object state).
//! Mutation of scrollback goes through [`ScrollbackPane`] methods only.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use phi_code_core::{KernelEvent, TickResult, TurnDriver};
use phi_code_ui::{
    AutoScrollDirection, HistoryScrollbar, HorizontalLayout, Scrollback, ScrollbackPainter,
    Selection, SystemClipboard,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Owns scrollback collaboration objects; Shell only routes messages here.
pub struct ScrollbackPane {
    scrollback: Scrollback,
    painter: ScrollbackPainter,
    selection: Selection,
    history_sb: HistoryScrollbar,
    clicks: ClickTracker,
    clipboard: SystemClipboard,
    view_h: usize,
    hit_pane: Rect,
    hit_content: Rect,
}

impl ScrollbackPane {
    #[must_use]
    pub fn new() -> Self {
        Self {
            scrollback: Scrollback::new(),
            painter: ScrollbackPainter::new(),
            selection: Selection::new(),
            history_sb: HistoryScrollbar::new(),
            clicks: ClickTracker::new(),
            clipboard: SystemClipboard::new(),
            view_h: 1,
            hit_pane: Rect::default(),
            hit_content: Rect::default(),
        }
    }

    #[must_use]
    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection.has_active()
    }

    /// Project one application tick onto the owned scrollback.
    ///
    /// Returns `true` when a generation finished (painter stream should clear).
    pub fn apply_tick(&mut self, driver: &TurnDriver, tick: &TickResult) -> bool {
        if tick.pump_failed {
            self.scrollback.end_live();
            self.resync_durable(driver);
        }
        if tick.lagged {
            self.resync_durable(driver);
            if !driver.is_busy() {
                self.scrollback.end_live();
            }
        }
        if tick.events.is_empty() && !tick.pump_failed && !tick.lagged {
            return false;
        }
        for ev in &tick.events {
            self.apply_event(driver, ev);
        }
        tick.generation_finished
    }

    /// After an accepted submit: durable history + open live overlay.
    pub fn apply_submit_accepted(&mut self, driver: &TurnDriver) {
        self.resync_durable(driver);
        self.scrollback.begin_live();
    }

    fn resync_durable(&mut self, driver: &TurnDriver) {
        if let Ok(items) = driver.history() {
            self.scrollback.set_durable(items);
        }
    }

    fn apply_event(&mut self, driver: &TurnDriver, ev: &KernelEvent) {
        match ev {
            KernelEvent::GenerationModelResponseCommitted { .. } => {
                self.scrollback.end_live();
                self.resync_durable(driver);
                self.scrollback.begin_live();
            }
            KernelEvent::GenerationStart { .. } => {
                if !self.scrollback.is_streaming() {
                    self.scrollback.begin_live();
                }
                self.resync_durable(driver);
            }
            KernelEvent::GenerationReasoningDelta { text, .. } => {
                self.scrollback.live_reasoning_delta(text);
            }
            KernelEvent::GenerationTextDelta { text, .. } => {
                if self.scrollback.live_text_delta(text) {
                    self.resync_durable(driver);
                }
            }
            KernelEvent::GenerationToolCall { .. }
            | KernelEvent::GenerationToolResult { .. }
            | KernelEvent::GenerationToolApprovalRequired { .. } => {
                self.resync_durable(driver);
            }
            KernelEvent::GenerationDone { .. }
            | KernelEvent::GenerationStopped { .. }
            | KernelEvent::GenerationError { .. } => {
                self.resync_durable(driver);
                self.scrollback.end_live();
            }
            KernelEvent::GenerationUsage { .. } | KernelEvent::GenerationAgentUnknown { .. } => {
                // Usage / unknown notes are application-side; nothing for the view.
            }
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Copy active character selection to the system clipboard.
    ///
    /// Returns char count on success.
    pub fn copy_selection(&mut self) -> Option<usize> {
        let text = self.selection.reconstruct_text(&self.scrollback)?;
        if text.is_empty() {
            return None;
        }
        let n = text.chars().count();
        self.clipboard.copy_text(&text);
        Some(n)
    }

    pub fn clear_stream_renderer(&mut self) {
        self.painter.clear_stream();
    }

    pub fn end_scrollbar_drag(&mut self) {
        self.history_sb.on_mouse_up();
    }

    /// Autoscroll while dragging + sync streaming markdown.
    pub fn tick(&mut self) {
        if self.selection.is_dragging()
            && let Some(auto) = self.selection.auto_scroll()
        {
            let delta = match auto.direction {
                AutoScrollDirection::Up => auto.speed as isize,
                AutoScrollDirection::Down => -(auto.speed as isize),
            };
            self.scrollback.scroll_by(delta, self.view_h);
        }
        self.painter.sync_stream(&self.scrollback);
    }

    pub fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, focused: bool) {
        self.hit_pane = area;
        let hint = if focused {
            "scrollback · drag select · click Thought · y copy · f fold"
        } else {
            "scrollback"
        };
        let [hint_row, pane] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(format!(" {hint} ")).style(Style::default().fg(Color::DarkGray)),
            hint_row,
        );

        self.view_h = pane.height.max(1) as usize;
        self.scrollback.prepare(
            HorizontalLayout::content_width_for(pane.width),
            self.view_h,
        );
        let (content, track) = HistoryScrollbar::split(pane, self.scrollback.total_height());
        self.hit_content = content;
        let layout_w = HorizontalLayout::content_width_for(content.width);
        self.scrollback.prepare(layout_w, self.view_h);
        self.painter
            .paint(frame, content, &self.scrollback, &mut self.selection);
        self.history_sb.paint(
            frame.buffer_mut(),
            track,
            self.scrollback.scroll_info(self.view_h),
        );
        if self.selection.is_dragging() {
            self.selection.refresh_drag_head_after_scroll();
        }
    }

    /// Returns true when the event was consumed by this pane.
    pub fn on_mouse(
        &mut self,
        mouse: MouseEvent,
        notify: &mut dyn FnMut(String),
    ) -> bool {
        // Continue an in-progress scrollbar drag even if the pointer leaves the track.
        if self.history_sb.is_dragging() {
            self.selection.clear();
            let info = self.scrollback.scroll_info(self.view_h);
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(off) = self.history_sb.on_mouse_drag(mouse.column, mouse.row, info)
                    {
                        self.scrollback.set_scroll_offset(off, self.view_h);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.history_sb.on_mouse_up();
                }
                _ => {}
            }
            return true;
        }

        if self.history_sb.contains(mouse.column, mouse.row) {
            let info = self.scrollback.scroll_info(self.view_h);
            return match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.selection.clear();
                    if let Some(off) = self.history_sb.on_mouse_down(mouse.column, mouse.row, info)
                    {
                        self.scrollback.set_scroll_offset(off, self.view_h);
                    }
                    true
                }
                MouseEventKind::ScrollUp => {
                    self.scrollback.scroll_by(1, self.view_h);
                    true
                }
                MouseEventKind::ScrollDown => {
                    self.scrollback.scroll_by(-1, self.view_h);
                    true
                }
                // Hover / move over track: ignore (must not steal prompt focus).
                _ => false,
            };
        }

        if !rect_contains(self.hit_content, mouse.column, mouse.row)
            && !rect_contains(self.hit_pane, mouse.column, mouse.row)
        {
            return false;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.history_sb.on_mouse_up();
                // Grok-style: click thinking header toggles expand/collapse.
                if let Some(hit) = self.selection.hit_test(mouse.column, mouse.row)
                    && self
                        .scrollback
                        .is_thinking_header(hit.entry_idx, hit.line_in_entry)
                {
                    let _ = self.clicks.register(mouse.column, mouse.row);
                    if self.scrollback.toggle_thinking(hit.entry_idx) {
                        self.selection.clear();
                        self.scrollback.select(hit.entry_idx);
                    }
                    return true;
                }
                let n = self.clicks.register(mouse.column, mouse.row);
                match n {
                    2 => {
                        self.selection.double_click_word(mouse.column, mouse.row);
                        self.copy_char_selection(notify, "word");
                    }
                    3 => {
                        self.selection.triple_click_line(mouse.column, mouse.row);
                        self.copy_char_selection(notify, "line");
                    }
                    _ => {
                        self.selection.on_mouse_down(mouse.column, mouse.row);
                        if let Some(hit) = self.selection.hit_test(mouse.column, mouse.row) {
                            self.scrollback.select(hit.entry_idx);
                        }
                    }
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.selection.on_mouse_drag(mouse.column, mouse.row);
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.selection.on_mouse_up().is_some() {
                    self.copy_char_selection(notify, "");
                }
                true
            }
            MouseEventKind::ScrollUp => {
                // Scroll history under the pointer without requiring scrollback focus.
                self.scrollback.scroll_by(1, self.view_h);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scrollback.scroll_by(-1, self.view_h);
                true
            }
            // Moved / other: do not claim the event (keeps prompt focus stable).
            _ => false,
        }
    }

    /// Scrollback-focused keys. Returns true when handled.
    ///
    /// `y` copies selection / entry. Ctrl+C copy is handled by the shell when
    /// a selection is active.
    pub fn on_key(&mut self, key: KeyEvent, notify: &mut dyn FnMut(String)) -> bool {
        match key.code {
            KeyCode::Char('y') => {
                if let Some(t) = self.selection.reconstruct_text(&self.scrollback) {
                    self.clipboard.copy_text(&t);
                    notify(format!("copied {} chars", t.chars().count()));
                } else if let Some(t) = self.scrollback.selected_plain_text() {
                    self.clipboard.copy_text(&t);
                    notify(format!("copied entry ({} chars)", t.chars().count()));
                } else {
                    notify("nothing selected".into());
                }
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.selection.clear();
                self.scrollback.select_delta(1, self.view_h);
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selection.clear();
                self.scrollback.select_delta(-1, self.view_h);
                true
            }
            KeyCode::Char('f') => {
                self.scrollback.toggle_fold_selected();
                true
            }
            _ => false,
        }
    }

    fn copy_char_selection(&mut self, notify: &mut dyn FnMut(String), kind: &str) {
        let Some(t) = self.selection.reconstruct_text(&self.scrollback) else {
            return;
        };
        self.clipboard.copy_text(&t);
        let n = t.chars().count();
        let msg = match kind {
            "word" => format!("copied word ({n} chars)"),
            "line" => format!("copied line ({n} chars)"),
            _ => format!("copied {n} chars"),
        };
        notify(msg);
    }
}

impl Default for ScrollbackPane {
    fn default() -> Self {
        Self::new()
    }
}

struct ClickTracker {
    last: Option<(u16, u16, Instant, u8)>,
}

impl ClickTracker {
    fn new() -> Self {
        Self { last: None }
    }

    fn register(&mut self, col: u16, row: u16) -> u8 {
        let now = Instant::now();
        let count = match self.last {
            Some((c, r, t, n))
                if c == col && r == row && now.duration_since(t) < Duration::from_millis(400) =>
            {
                (n + 1).min(3)
            }
            _ => 1,
        };
        self.last = Some((col, row, now, count));
        count
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}
