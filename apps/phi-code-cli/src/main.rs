//! phi-code CLI — TUI skeleton with Grok-like vertical panes.
//!
//! Layout (from Grok `AgentViewLayout`, simplified): status → scrollback → prompt → shortcuts.
//! Prompt: cursor edit, Enter send, paste, Ctrl+M multiline, Tab focus, mouse hit-test.

use std::io::{self, stdout};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

enum Focus {
    Prompt,
    Scrollback,
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;

    let mut result = Ok(());
    let mut text = String::new();
    // Cursor as Unicode scalar index into `text` (`0..=text.chars().count()`).
    let mut cursor: usize = 0;
    let mut multiline = false;
    let mut focus = Focus::Prompt;
    let mut scrollback_lines: Vec<String> = Vec::new();
    // Lines scrolled up from the bottom of scrollback (0 = stick to end).
    let mut sb_scroll: usize = 0;

    let mut hit_scrollback = Rect::default();
    let mut hit_prompt = Rect::default();
    let mut hit_prompt_inner = Rect::default();
    let mut hit_sb_inner = Rect::default();

    loop {
        if let Err(err) = terminal.draw(|frame| {
            let content_lines = if text.is_empty() {
                1u16
            } else {
                u16::try_from(text.split('\n').count()).unwrap_or(1).max(1)
            };
            let prompt_h = if multiline {
                (content_lines.saturating_add(2)).clamp(5, 12)
            } else {
                (content_lines.saturating_add(2)).clamp(3, 8)
            };

            let [status, scrollback, prompt, shortcuts] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(prompt_h),
                Constraint::Length(1),
            ])
            .areas(frame.area());

            hit_scrollback = scrollback;
            hit_prompt = prompt;

            let mode = if multiline { "multiline" } else { "single" };
            let focus_label = match focus {
                Focus::Prompt => "prompt",
                Focus::Scrollback => "scrollback",
            };
            frame.render_widget(
                Paragraph::new(format!(" focus:{focus_label}  {mode} ")),
                status,
            );

            let sb_title = match focus {
                Focus::Scrollback => " scrollback * ",
                Focus::Prompt => " scrollback ",
            };
            let sb_block = Block::default().borders(Borders::ALL).title(sb_title);
            let sb_inner = sb_block.inner(scrollback);
            hit_sb_inner = sb_inner;
            frame.render_widget(sb_block, scrollback);

            let view_h = sb_inner.height as usize;
            let total = scrollback_lines.len();
            let max_scroll = total.saturating_sub(view_h.max(1));
            if sb_scroll > max_scroll {
                sb_scroll = max_scroll;
            }
            let end = total.saturating_sub(sb_scroll);
            let start = end.saturating_sub(view_h);
            let sb_body = if scrollback_lines.is_empty() {
                "scrollback (empty)".to_owned()
            } else {
                scrollback_lines[start..end].join("\n")
            };
            frame.render_widget(Paragraph::new(sb_body), sb_inner);

            let pr_title = match focus {
                Focus::Prompt => " prompt * ",
                Focus::Scrollback => " prompt ",
            };
            let prompt_block = Block::default().borders(Borders::ALL).title(pr_title);
            let prompt_inner = prompt_block.inner(prompt);
            hit_prompt_inner = prompt_inner;
            frame.render_widget(prompt_block, prompt);
            frame.render_widget(Paragraph::new(text.as_str()), prompt_inner);

            if matches!(focus, Focus::Prompt)
                && prompt_inner.width > 0
                && prompt_inner.height > 0
            {
                // Display column = Unicode width (Grok: display_width_of_range), not char count.
                let before: String = text.chars().take(cursor).collect();
                let row = before.bytes().filter(|&b| b == b'\n').count() as u16;
                let col = before
                    .rsplit('\n')
                    .next()
                    .map(|l| l.width() as u16)
                    .unwrap_or(0);
                let cx = prompt_inner
                    .x
                    .saturating_add(col)
                    .min(prompt_inner.x.saturating_add(prompt_inner.width.saturating_sub(1)));
                let cy = prompt_inner
                    .y
                    .saturating_add(row)
                    .min(prompt_inner.y.saturating_add(prompt_inner.height.saturating_sub(1)));
                frame.set_cursor_position((cx, cy));
            }

            frame.render_widget(
                Paragraph::new(
                    " click panes · wheel scroll · tab focus · enter send · C-m multiline · esc quit ",
                ),
                shortcuts,
            );
        }) {
            result = Err(err);
            break;
        }

        match event::read() {
            Ok(Event::Paste(payload)) if matches!(focus, Focus::Prompt) => {
                let normalized = payload.replace("\r\n", "\n").replace('\r', "\n");
                let byte = char_to_byte(&text, cursor);
                text.insert_str(byte, &normalized);
                cursor = cursor.saturating_add(normalized.chars().count());
            }
            Ok(Event::Mouse(mouse)) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if rect_contains(hit_prompt, mouse.column, mouse.row) {
                        focus = Focus::Prompt;
                        if rect_contains(hit_prompt_inner, mouse.column, mouse.row) {
                            let col = mouse.column.saturating_sub(hit_prompt_inner.x) as usize;
                            let row = mouse.row.saturating_sub(hit_prompt_inner.y) as usize;
                            cursor = cursor_at_click(&text, col, row);
                        }
                    } else if rect_contains(hit_scrollback, mouse.column, mouse.row) {
                        focus = Focus::Scrollback;
                    }
                }
                MouseEventKind::ScrollUp
                    if rect_contains(hit_scrollback, mouse.column, mouse.row)
                        || rect_contains(hit_sb_inner, mouse.column, mouse.row) =>
                {
                    sb_scroll = sb_scroll.saturating_add(1);
                }
                MouseEventKind::ScrollDown
                    if rect_contains(hit_scrollback, mouse.column, mouse.row)
                        || rect_contains(hit_sb_inner, mouse.column, mouse.row) =>
                {
                    sb_scroll = sb_scroll.saturating_sub(1);
                }
                _ => {}
            },
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);

                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Tab => {
                        focus = match focus {
                            Focus::Prompt => Focus::Scrollback,
                            Focus::Scrollback => Focus::Prompt,
                        };
                    }
                    KeyCode::Char('m') if ctrl && matches!(focus, Focus::Prompt) => {
                        multiline = !multiline;
                    }
                    KeyCode::Enter if matches!(focus, Focus::Prompt) => {
                        if shift || alt {
                            let byte = char_to_byte(&text, cursor);
                            text.insert(byte, '\n');
                            cursor = cursor.saturating_add(1);
                        } else if cursor == text.chars().count() && text.ends_with('\\') {
                            text.pop();
                            cursor = cursor.saturating_sub(1);
                            let byte = char_to_byte(&text, cursor);
                            text.insert(byte, '\n');
                            cursor = cursor.saturating_add(1);
                        } else {
                            let msg = text.trim().to_owned();
                            if !msg.is_empty() {
                                scrollback_lines.push(format!("› {msg}"));
                                text.clear();
                                cursor = 0;
                                sb_scroll = 0;
                            }
                        }
                    }
                    KeyCode::Left if matches!(focus, Focus::Prompt) && cursor > 0 => {
                        cursor -= 1;
                    }
                    KeyCode::Right
                        if matches!(focus, Focus::Prompt) && cursor < text.chars().count() =>
                    {
                        cursor += 1;
                    }
                    KeyCode::Home if matches!(focus, Focus::Prompt) => {
                        let before: String = text.chars().take(cursor).collect();
                        let line_start = before.rfind('\n').map(|i| before[..=i].chars().count());
                        cursor = line_start.unwrap_or(0);
                    }
                    KeyCode::End if matches!(focus, Focus::Prompt) => {
                        let total = text.chars().count();
                        let after: String = text.chars().skip(cursor).collect();
                        if let Some(rel) = after.find('\n') {
                            cursor += after[..rel].chars().count();
                        } else {
                            cursor = total;
                        }
                    }
                    KeyCode::Backspace if matches!(focus, Focus::Prompt) && cursor > 0 => {
                        let start = char_to_byte(&text, cursor - 1);
                        let end = char_to_byte(&text, cursor);
                        text.replace_range(start..end, "");
                        cursor -= 1;
                    }
                    KeyCode::Delete
                        if matches!(focus, Focus::Prompt) && cursor < text.chars().count() =>
                    {
                        let start = char_to_byte(&text, cursor);
                        let end = char_to_byte(&text, cursor + 1);
                        text.replace_range(start..end, "");
                    }
                    KeyCode::Char(c)
                        if matches!(focus, Focus::Prompt)
                            && !ctrl
                            && !alt
                            && !c.is_control() =>
                    {
                        let byte = char_to_byte(&text, cursor);
                        text.insert(byte, c);
                        cursor += 1;
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

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x.saturating_add(r.width) && row >= r.y && row < r.y.saturating_add(r.height)
}

/// Map a click in the prompt content box to a char index (logical lines only; no soft-wrap yet).
///
/// Matches Grok `TextArea::display_col_to_buffer_pos` (plain-text arm): walk
/// **grapheme clusters**, accumulate **Unicode display width**, and when the
/// click column falls inside a cluster, snap to that cluster's **start**
/// (`width_so_far > target_col` → return position before the cluster).
/// See `xai-ratatui-textarea` `textarea.rs` `display_col_to_buffer_pos`.
fn cursor_at_click(text: &str, target_col: usize, row: usize) -> usize {
    let mut line_byte = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i == row {
            let line_end = line_byte + line.len();
            let mut pos = line_byte;
            let mut width_so_far = 0usize;
            while pos < line_end {
                let Some(grapheme) = text[pos..line_end].graphemes(true).next() else {
                    break;
                };
                let grapheme_width = grapheme.width();
                width_so_far = width_so_far.saturating_add(grapheme_width);
                if width_so_far > target_col {
                    // Snap to start of this grapheme (Grok plain-text behavior).
                    return text[..pos].chars().count();
                }
                pos += grapheme.len();
            }
            return text[..line_end].chars().count();
        }
        line_byte = line_byte.saturating_add(line.len()).saturating_add(1); // + '\n'
    }
    text.chars().count()
}
