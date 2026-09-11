//! Scrollback painter object: accent bar + content + selection geometry.

use phi_kernel::TurnItem;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Frame;
use xai_grok_markdown::StreamingMarkdownRenderer;

use crate::layout::HorizontalLayout;
use crate::markdown::ProductMarkdown;
use crate::scrollback::{Accent, Scrollback, VisibleSegment};
use crate::selection::{SelectableLine, Selection};

/// Thinking / `TurnItem::Reasoning` foreground.
///
/// Keep this subdued. **Do not** use `Color::Yellow` / `LightYellow` — it steals
/// focus from the assistant answer. Header and body share this dim palette.
const THINKING_FG: Color = Color::DarkGray;

/// Paints scrollback into a frame and feeds the selection line map.
///
/// Owns streaming markdown state so stream deltas and paint stay coupled.
#[derive(Default)]
pub struct ScrollbackPainter {
    markdown: ProductMarkdown,
    stream_entry: Option<usize>,
    stream_fed_bytes: usize,
    stream_renderer: Option<StreamingMarkdownRenderer>,
}

impl ScrollbackPainter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            markdown: ProductMarkdown::new(),
            stream_entry: None,
            stream_fed_bytes: 0,
            stream_renderer: None,
        }
    }

    /// Drop streaming renderer (after stream ends or user clears).
    pub fn clear_stream(&mut self) {
        self.stream_entry = None;
        self.stream_fed_bytes = 0;
        self.stream_renderer = None;
    }

    /// Sync streaming renderer with the scrollback's active **assistant** entry.
    /// Reasoning rows do not use the markdown stream renderer.
    pub fn sync_stream(&mut self, scrollback: &Scrollback) {
        let Some(i) = scrollback.streaming_index() else {
            self.clear_stream();
            return;
        };
        let TurnItem::Assistant { content } = &scrollback.items()[i] else {
            // Streaming a Reasoning (or other) row — no md stream.
            self.clear_stream();
            return;
        };
        if self.stream_entry != Some(i) {
            self.stream_entry = Some(i);
            self.stream_fed_bytes = 0;
            self.stream_renderer = Some(self.markdown.streaming_renderer());
        }
        let Some(renderer) = self.stream_renderer.as_mut() else {
            return;
        };
        if content.len() > self.stream_fed_bytes {
            let chunk = &content[self.stream_fed_bytes..];
            renderer.push_and_render(chunk, None);
            self.stream_fed_bytes = content.len();
        }
    }

    /// Paint the scrollback pane and register selectable lines on `selection`.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        scrollback: &Scrollback,
        selection: &mut Selection,
    ) {
        selection.begin_frame(area);

        if area.width == 0 || area.height == 0 {
            return;
        }
        if scrollback.is_empty() {
            frame.render_widget(
                Paragraph::new("scrollback empty — history is kernel TurnItem")
                    .style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }

        let segs = scrollback.visible_segments(area.height as usize);
        let mut y = area.y;
        for seg in segs {
            let h = seg.visible_rows as u16;
            if h == 0 || y >= area.y + area.height {
                break;
            }
            let h = h.min(area.y + area.height - y);
            let entry_area = Rect::new(area.x, y, area.width, h);
            self.paint_segment(frame, entry_area, scrollback, &seg, selection);
            y = y.saturating_add(h);
        }

        selection.paint_highlight(frame.buffer_mut());
    }

    fn paint_segment(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        scrollback: &Scrollback,
        seg: &VisibleSegment,
        selection: &mut Selection,
    ) {
        if area.width < 2 {
            return;
        }
        let cols = HorizontalLayout::for_area(area);

        let bar = "│".repeat(cols.accent().height as usize);
        Paragraph::new(bar)
            .style(Style::default().fg(self.accent_color(seg.accent)))
            .render(cols.accent(), frame.buffer_mut());

        let content = cols.content();
        if content.width == 0 || content.height == 0 {
            return;
        }

        let width = content.width.max(1) as usize;
        let (paint_lines, plain_lines) = self.resolve_lines(scrollback, seg, width);

        let start = seg.clip_top.min(paint_lines.len());
        let end = (start + seg.visible_rows).min(paint_lines.len());
        let slice = paint_lines[start..end].to_vec();

        if slice.is_empty() {
            Paragraph::new("…")
                .style(Style::default().fg(Color::DarkGray))
                .render(content, frame.buffer_mut());
        } else {
            Paragraph::new(slice).render(content, frame.buffer_mut());
        }

        for (row_i, plain) in plain_lines
            .iter()
            .enumerate()
            .skip(seg.clip_top)
            .take(seg.visible_rows)
        {
            let screen_y = content.y.saturating_add((row_i - seg.clip_top) as u16);
            if screen_y >= content.y.saturating_add(content.height) {
                break;
            }
            let w = SelectableLine::display_width(plain).min(content.width);
            selection.register_line(SelectableLine {
                entry_idx: seg.entry_index,
                line_in_entry: row_i,
                screen_y,
                screen_x: content.x,
                width: w,
                text: plain.clone(),
            });
        }
    }

    fn resolve_lines(
        &self,
        scrollback: &Scrollback,
        seg: &VisibleSegment,
        width: usize,
    ) -> (Vec<Line<'static>>, Vec<String>) {
        match &seg.item {
            TurnItem::Reasoning { .. } if !seg.folded => {
                let plains = scrollback.entry_lines(seg.entry_index);
                let lay = scrollback.thinking_layout(seg.entry_index, width);
                let paint: Vec<Line<'static>> = plains
                    .iter()
                    .enumerate()
                    .map(|(i, plain)| {
                        if lay.is_some_and(|t| t.is_header_row(i)) {
                            // Subdued chrome — see THINKING_FG (not Yellow).
                            Line::from(Span::styled(
                                plain.clone(),
                                Style::default().fg(THINKING_FG),
                            ))
                        } else {
                            Line::from(Span::styled(
                                plain.clone(),
                                Style::default()
                                    .fg(THINKING_FG)
                                    .add_modifier(ratatui::style::Modifier::DIM)
                                    .add_modifier(ratatui::style::Modifier::ITALIC),
                            ))
                        }
                    })
                    .collect();
                (paint, plains)
            }
            TurnItem::Assistant { content } if !seg.folded => {
                let md = if seg.streaming {
                    self.stream_lines_for(seg.entry_index, width)
                        .unwrap_or_else(|| self.markdown.pretty_lines(content, width))
                } else {
                    self.markdown.pretty_lines(content, width)
                };
                let plains = if seg.streaming {
                    md.iter().map(ProductMarkdown::plain_of_line).collect()
                } else {
                    scrollback.entry_lines(seg.entry_index)
                };
                (md, plains)
            }
            _ => {
                let plains = scrollback.entry_lines(seg.entry_index);
                let styled: Vec<Line<'static>> = plains
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let fg = match &seg.item {
                            TurnItem::User { .. } => Color::Cyan,
                            // Subdued — see THINKING_FG (not Yellow).
                            TurnItem::Reasoning { .. } => THINKING_FG,
                            TurnItem::ToolCall { .. } | TurnItem::ToolResult { .. } if i == 0 => {
                                Color::Magenta
                            }
                            TurnItem::ToolCall { .. } | TurnItem::ToolResult { .. } => Color::Gray,
                            TurnItem::Assistant { .. } => Color::Green,
                            TurnItem::Continuation { .. } | TurnItem::ModelResponse { .. } => {
                                Color::Gray
                            }
                        };
                        Line::from(Span::styled(t.clone(), Style::default().fg(fg)))
                    })
                    .collect();
                (styled, plains)
            }
        }
    }

    fn stream_lines_for(&self, entry: usize, width: usize) -> Option<Vec<Line<'static>>> {
        if self.stream_entry != Some(entry) {
            return None;
        }
        let view = self.stream_renderer.as_ref()?.view();
        Some(ProductMarkdown::wrap_lines(&view.lines, width))
    }

    fn accent_color(&self, accent: Accent) -> Color {
        match accent {
            Accent::User => Color::Cyan,
            Accent::Assistant => Color::Green,
            // Subdued gutter — see THINKING_FG (not Yellow).
            Accent::Thinking => THINKING_FG,
            Accent::ToolRunning => Color::Blue,
            Accent::ToolOk => Color::Green,
            Accent::ToolError => Color::Red,
            Accent::ToolOther => Color::Yellow,
        }
    }
}
