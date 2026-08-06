//! phi-code CLI — Grok-like layout; prompt powered by vendored `xai-ratatui-textarea`.

use std::io::{self, stdout};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidgetRef};
use xai_ratatui_textarea::{TextArea, TextAreaState};

enum Focus {
    Prompt,
    Scrollback,
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;

    let mut result = Ok(());
    let mut textarea = TextArea::new();
    let mut ta_state = TextAreaState::default();
    let mut ta_area = Rect::default();
    let mut multiline_prefer = false;
    let mut focus = Focus::Prompt;
    let mut scrollback_lines: Vec<String> = Vec::new();
    let mut sb_scroll: usize = 0;
    let mut hit_scrollback = Rect::default();
    let mut hit_prompt = Rect::default();
    let mut last_inner_w: u16 = 40;

    loop {
        if let Err(err) = terminal.draw(|frame| {
            let content_h = textarea
                .desired_height(last_inner_w.max(1))
                .clamp(1, if multiline_prefer { 10 } else { 6 });
            let prompt_h = content_h.saturating_add(2).clamp(3, 12);

            let [status, scrollback, prompt, shortcuts] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(prompt_h),
                Constraint::Length(1),
            ])
            .areas(frame.area());

            hit_scrollback = scrollback;
            hit_prompt = prompt;

            let mode = if multiline_prefer { "multiline" } else { "single" };
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
                    " textarea vendor · click/wheel · tab focus · enter send · C-m expand · esc quit ",
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
                textarea.insert_str(&normalized);
            }
            Ok(Event::Mouse(mouse)) => {
                if rect_contains(hit_prompt, mouse.column, mouse.row) {
                    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                        focus = Focus::Prompt;
                    }
                    if matches!(focus, Focus::Prompt) {
                        let _ = textarea.handle_mouse(mouse, ta_area, ta_state);
                    }
                } else if rect_contains(hit_scrollback, mouse.column, mouse.row) {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            focus = Focus::Scrollback;
                        }
                        MouseEventKind::ScrollUp => {
                            sb_scroll = sb_scroll.saturating_add(1);
                        }
                        MouseEventKind::ScrollDown => {
                            sb_scroll = sb_scroll.saturating_sub(1);
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
                    KeyCode::Esc => break,
                    KeyCode::Tab => {
                        focus = match focus {
                            Focus::Prompt => Focus::Scrollback,
                            Focus::Scrollback => Focus::Prompt,
                        };
                    }
                    KeyCode::Char('m') if ctrl => {
                        // Host expand preference (do not forward: TextArea maps C-m to newline).
                        multiline_prefer = !multiline_prefer;
                    }
                    KeyCode::Enter if matches!(focus, Focus::Prompt) => {
                        if shift || alt {
                            textarea.insert_str("\n");
                        } else {
                            let msg = textarea.text().trim().to_owned();
                            if !msg.is_empty() {
                                scrollback_lines.push(format!("› {msg}"));
                                textarea.set_text("");
                                sb_scroll = 0;
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
