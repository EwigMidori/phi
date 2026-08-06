//! phi-code CLI — thin coordinator over `phi-code-ui` objects.

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use phi_code_ui::{
    AutoScrollDirection, HistoryScrollbar, HorizontalLayout, PastePolicy, Scrollback,
    ScrollbackPainter, Selection, SystemClipboard,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidgetRef};
use xai_ratatui_textarea::{TextArea, TextAreaState};

enum Focus {
    Prompt,
    Scrollback,
}

struct PendingStream {
    rest: String,
}

/// Double/triple-click tracker (private coordinator state).
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

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;

    let mut result = Ok(());
    let mut textarea = TextArea::new();
    textarea.set_clipboard_provider(Box::new(SystemClipboard::new()));
    let mut ta_state = TextAreaState::default();
    let mut ta_area = Rect::default();
    let mut multiline_prefer = false;
    let mut focus = Focus::Prompt;

    // UI objects (Kay): each owns its state; main only sends messages.
    let mut scrollback = Scrollback::new();
    let mut painter = ScrollbackPainter::new();
    let mut selection = Selection::new();
    let mut history_sb = HistoryScrollbar::new();
    let paste_policy = PastePolicy::new();
    let mut clipboard = SystemClipboard::new();

    let mut pending: Option<PendingStream> = None;
    let mut clicks = ClickTracker::new();
    let mut hit_scrollback = Rect::default();
    let mut hit_sb_content = Rect::default();
    let mut hit_prompt = Rect::default();
    let mut last_inner_w: u16 = 40;
    let mut sb_view_h: usize = 1;
    let mut status_note = String::new();

    loop {
        if let Some(p) = pending.as_mut() {
            let n = p.rest.chars().take(12).map(|c| c.len_utf8()).sum::<usize>();
            if n == 0 {
                scrollback.finish_assistant_stream();
                painter.clear_stream();
                pending = None;
            } else {
                let take = n.min(p.rest.len());
                let (chunk, rest) = p.rest.split_at(take);
                let chunk = chunk.to_owned();
                p.rest = rest.to_owned();
                scrollback.append_assistant_delta(&chunk);
                if p.rest.is_empty() {
                    scrollback.finish_assistant_stream();
                    painter.clear_stream();
                    pending = None;
                }
            }
            scrollback.scroll_to_bottom();
        }

        if selection.is_dragging()
            && let Some(auto) = selection.auto_scroll()
        {
            let delta = match auto.direction {
                AutoScrollDirection::Up => auto.speed as isize,
                AutoScrollDirection::Down => -(auto.speed as isize),
            };
            scrollback.scroll_by(delta, sb_view_h);
        }

        painter.sync_stream(&scrollback);

        if let Err(err) = terminal.draw(|frame| {
            let content_h = textarea
                .desired_height(last_inner_w.max(1))
                .clamp(1, if multiline_prefer { 10 } else { 6 });
            let prompt_h = content_h.saturating_add(2).clamp(3, 12);

            let [status, sb_rect, prompt, shortcuts] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(prompt_h),
                Constraint::Length(1),
            ])
            .areas(frame.area());

            hit_scrollback = sb_rect;
            hit_prompt = prompt;

            let mode = if multiline_prefer { "multiline" } else { "single" };
            let stream = if scrollback.is_streaming() {
                "streaming"
            } else {
                "idle"
            };
            let focus_label = match focus {
                Focus::Prompt => "prompt",
                Focus::Scrollback => "scrollback",
            };
            let note = if status_note.is_empty() {
                String::new()
            } else {
                format!("  {status_note}")
            };
            frame.render_widget(
                Paragraph::new(format!(
                    " focus:{focus_label}  {mode}  {stream}  turns:{}  h:{}{note} ",
                    scrollback.items().len(),
                    scrollback.total_height()
                )),
                status,
            );

            let sb_hint = match focus {
                Focus::Scrollback => {
                    "scrollback · drag select · scrollbar · dbl=word · y copy · f fold"
                }
                Focus::Prompt => "scrollback",
            };
            let [sb_hint_row, sb_pane] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(sb_rect);
            frame.render_widget(
                Paragraph::new(format!(" {sb_hint} ")).style(Style::default().fg(Color::DarkGray)),
                sb_hint_row,
            );

            // Reserve gap+track when history overflows (Grok HistoryScrollbar).
            // Height first with full width estimate, then split, then prepare at
            // content width so line wrap matches painted content.
            sb_view_h = sb_pane.height.max(1) as usize;
            scrollback.prepare(
                HorizontalLayout::content_width_for(sb_pane.width),
                sb_view_h,
            );
            let (sb_content, sb_track) =
                HistoryScrollbar::split(sb_pane, scrollback.total_height());
            hit_sb_content = sb_content;
            let layout_w = HorizontalLayout::content_width_for(sb_content.width);
            scrollback.prepare(layout_w, sb_view_h);
            painter.paint(frame, sb_content, &scrollback, &mut selection);
            history_sb.paint(
                frame.buffer_mut(),
                sb_track,
                scrollback.scroll_info(sb_view_h),
            );

            if selection.is_dragging() {
                selection.refresh_drag_head_after_scroll();
            }

            let pr_title = match focus {
                Focus::Prompt => "prompt * ",
                Focus::Scrollback => "prompt ",
            };
            let prompt_block = Block::default()
                .borders(Borders::TOP)
                .title(pr_title)
                .border_style(Style::default().fg(Color::DarkGray));
            let prompt_inner = prompt_block.inner(prompt);
            frame.render_widget(prompt_block, prompt);
            last_inner_w = prompt_inner.width;
            ta_area = prompt_inner;

            if prompt_inner.width > 0 && prompt_inner.height > 0 {
                StatefulWidgetRef::render_ref(
                    &&textarea,
                    prompt_inner,
                    frame.buffer_mut(),
                    &mut ta_state,
                );
                if matches!(focus, Focus::Prompt)
                    && let Some((cx, cy)) = textarea.cursor_pos_with_state(prompt_inner, ta_state)
                {
                    frame.set_cursor_position(ratatui::layout::Position { x: cx, y: cy });
                }
            }

            frame.render_widget(
                Paragraph::new(
                    " select · scrollbar · CJK · pretty md · paste chip · C-v · y/C-c · esc ",
                )
                .style(Style::default().fg(Color::DarkGray)),
                shortcuts,
            );
        }) {
            result = Err(err);
            break;
        }

        let has_event = event::poll(Duration::from_millis(16))?;
        if !has_event {
            continue;
        }

        match event::read() {
            Ok(Event::Paste(payload)) if matches!(focus, Focus::Prompt) => {
                paste_policy.apply(&mut textarea, &payload);
            }
            Ok(Event::Mouse(mouse)) => {
                if rect_contains(hit_prompt, mouse.column, mouse.row) {
                    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                        focus = Focus::Prompt;
                        selection.clear();
                        history_sb.on_mouse_up();
                    }
                    if matches!(focus, Focus::Prompt) {
                        let _ = textarea.handle_mouse(mouse, ta_area, ta_state);
                    }
                } else if history_sb.is_dragging()
                    || history_sb.contains(mouse.column, mouse.row)
                {
                    // Scrollbar grabs first (Grok): click/drag jumps offset.
                    focus = Focus::Scrollback;
                    selection.clear();
                    let info = scrollback.scroll_info(sb_view_h);
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some(off) =
                                history_sb.on_mouse_down(mouse.column, mouse.row, info)
                            {
                                scrollback.set_scroll_offset(off, sb_view_h);
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            if let Some(off) =
                                history_sb.on_mouse_drag(mouse.column, mouse.row, info)
                            {
                                scrollback.set_scroll_offset(off, sb_view_h);
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            history_sb.on_mouse_up();
                        }
                        MouseEventKind::ScrollUp => {
                            scrollback.scroll_by(1, sb_view_h);
                        }
                        MouseEventKind::ScrollDown => {
                            scrollback.scroll_by(-1, sb_view_h);
                        }
                        _ => {}
                    }
                } else if rect_contains(hit_sb_content, mouse.column, mouse.row)
                    || rect_contains(hit_scrollback, mouse.column, mouse.row)
                {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            focus = Focus::Scrollback;
                            history_sb.on_mouse_up();
                            let n = clicks.register(mouse.column, mouse.row);
                            match n {
                                2 => {
                                    selection.double_click_word(mouse.column, mouse.row);
                                    if let Some(t) = selection.reconstruct_text(&scrollback) {
                                        clipboard.copy_text(&t);
                                        status_note =
                                            format!("copied word ({} chars)", t.chars().count());
                                    }
                                }
                                3 => {
                                    selection.triple_click_line(mouse.column, mouse.row);
                                    if let Some(t) = selection.reconstruct_text(&scrollback) {
                                        clipboard.copy_text(&t);
                                        status_note =
                                            format!("copied line ({} chars)", t.chars().count());
                                    }
                                }
                                _ => {
                                    selection.on_mouse_down(mouse.column, mouse.row);
                                    if let Some(hit) = selection.hit_test(mouse.column, mouse.row)
                                    {
                                        scrollback.select(hit.entry_idx);
                                    }
                                }
                            }
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            focus = Focus::Scrollback;
                            selection.on_mouse_drag(mouse.column, mouse.row);
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if selection.on_mouse_up().is_some() {
                                if let Some(t) = selection.reconstruct_text(&scrollback) {
                                    clipboard.copy_text(&t);
                                    status_note = format!("copied {} chars", t.chars().count());
                                }
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            scrollback.scroll_by(1, sb_view_h);
                        }
                        MouseEventKind::ScrollDown => {
                            scrollback.scroll_by(-1, sb_view_h);
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);

                match key.code {
                    KeyCode::Esc => {
                        if selection.has_active() {
                            selection.clear();
                            status_note.clear();
                        } else {
                            break;
                        }
                    }
                    KeyCode::Tab => {
                        focus = match focus {
                            Focus::Prompt => Focus::Scrollback,
                            Focus::Scrollback => Focus::Prompt,
                        };
                    }
                    KeyCode::Char('m') if ctrl => {
                        multiline_prefer = !multiline_prefer;
                    }
                    KeyCode::Char('v' | 'V') if ctrl && matches!(focus, Focus::Prompt) => {
                        if let Some(text) = clipboard.paste_text() {
                            paste_policy.apply(&mut textarea, &text);
                            status_note = "pasted".into();
                        }
                    }
                    KeyCode::Char('c' | 'C') if ctrl && matches!(focus, Focus::Scrollback) => {
                        if let Some(t) = selection.reconstruct_text(&scrollback) {
                            clipboard.copy_text(&t);
                            status_note = format!("copied {} chars", t.chars().count());
                        }
                    }
                    KeyCode::Char('y') if matches!(focus, Focus::Scrollback) => {
                        if let Some(t) = selection.reconstruct_text(&scrollback) {
                            clipboard.copy_text(&t);
                            status_note = format!("copied {} chars", t.chars().count());
                        } else if let Some(t) = scrollback.selected_plain_text() {
                            clipboard.copy_text(&t);
                            status_note = format!("copied entry ({} chars)", t.chars().count());
                        } else {
                            status_note = "nothing selected".into();
                        }
                    }
                    KeyCode::Char('j') | KeyCode::Down if matches!(focus, Focus::Scrollback) => {
                        selection.clear();
                        scrollback.select_delta(1, sb_view_h);
                    }
                    KeyCode::Char('k') | KeyCode::Up if matches!(focus, Focus::Scrollback) => {
                        selection.clear();
                        scrollback.select_delta(-1, sb_view_h);
                    }
                    KeyCode::Char('f') if matches!(focus, Focus::Scrollback) => {
                        scrollback.toggle_fold_selected();
                    }
                    KeyCode::Enter if matches!(focus, Focus::Prompt) => {
                        if shift || alt {
                            textarea.insert_str("\n");
                        } else if pending.is_none() && !scrollback.is_streaming() {
                            let msg = textarea.text().trim().to_owned();
                            if !msg.is_empty() {
                                scrollback.push_user(&msg);
                                let body = format!(
                                    "### echo\n\nYou said:\n\n```\n{msg}\n```\n\n*streaming…*\n"
                                );
                                scrollback.begin_assistant_stream();
                                pending = Some(PendingStream { rest: body });
                                painter.clear_stream();
                                selection.clear();
                                textarea.set_text("");
                                scrollback.scroll_to_bottom();
                            }
                        }
                    }
                    _ if matches!(focus, Focus::Prompt) => {
                        textarea.input(key);
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(err) => {
                result = Err(err);
                break;
            }
        }
    }

    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}
