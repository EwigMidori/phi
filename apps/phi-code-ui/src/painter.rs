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

    /// Sync streaming renderer with the scrollback's active assistant entry.
    pub fn sync_stream(&mut self, scrollback: &Scrollback) {
        let Some(i) = scrollback.streaming_index() else {
            self.clear_stream();
            return;
        };
        let TurnItem::Assistant { content } = &scrollback.items()[i] else {
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
            TurnItem::Assistant { content } if !seg.folded => {
                let plains = scrollback.entry_lines(seg.entry_index);
                let think = scrollback.thinking_layout(seg.entry_index, width);
                let think_rows = think.map(crate::scrollback::ThinkingLayout::total_rows).unwrap_or(0);

                let body_md = if content.is_empty() && think_rows > 0 {
                    Vec::new()
                } else if seg.streaming {
                    self.stream_lines_for(seg.entry_index, width)
                        .unwrap_or_else(|| self.markdown.pretty_lines(content, width))
                } else if content.is_empty() {
                    vec![Line::from("")]
                } else {
                    self.markdown.pretty_lines(content, width)
                };

                // Prefix thinking lines (header + optional body) so paint ≡ plain.
                let mut paint: Vec<Line<'static>> = Vec::with_capacity(plains.len());
                for (i, plain) in plains.iter().enumerate() {
                    if think.is_some_and(|t| t.is_header_row(i)) {
                        paint.push(Line::from(Span::styled(
                            plain.clone(),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        )));
                    } else if think.is_some_and(|t| t.is_body_row(i)) {
                        paint.push(Line::from(Span::styled(
                            plain.clone(),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(ratatui::style::Modifier::DIM)
                                .add_modifier(ratatui::style::Modifier::ITALIC),
                        )));
                    } else {
                        // Body: use pretty md when available, else plain.
                        let body_i = i.saturating_sub(think_rows);
                        if let Some(line) = body_md.get(body_i) {
                            paint.push(line.clone());
                        } else {
                            paint.push(Line::from(Span::styled(
                                plain.clone(),
                                Style::default().fg(Color::Green),
                            )));
                        }
                    }
                }
                // Keep lengths aligned for clip/selection.
                while paint.len() < plains.len() {
                    paint.push(Line::from(""));
                }
                if paint.len() > plains.len() {
                    paint.truncate(plains.len());
                }
                (paint, plains)
            }
            _ => {
                let plains = scrollback.entry_lines(seg.entry_index);
                let styled: Vec<Line<'static>> = plains
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let fg = match &seg.item {
                            TurnItem::User { .. } => Color::Cyan,
                            TurnItem::ToolCall { .. } | TurnItem::ToolResult { .. } if i == 0 => {
                                Color::Magenta
                            }
                            TurnItem::ToolCall { .. } | TurnItem::ToolResult { .. } => Color::Gray,
                            TurnItem::Assistant { .. } => Color::Green,
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
            Accent::Thinking => Color::Yellow,
            Accent::ToolRunning => Color::Blue,
            Accent::ToolOk => Color::Green,
            Accent::ToolError => Color::Red,
            Accent::ToolOther => Color::Yellow,
        }
    }
}
