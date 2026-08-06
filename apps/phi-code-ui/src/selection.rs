//! Character-level selection object (Grok text_selection principles).
//!
//! Private geometry + drag state; collaborators send mouse messages and ask
//! for reconstruct / highlight.

use std::cmp::{max, min};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Provider of full entry display lines (usually [`crate::Scrollback`]).
pub trait LineSource {
    fn lines_of(&self, entry_idx: usize) -> Vec<String>;
}

/// One visible selectable display line after layout.
#[derive(Debug, Clone)]
pub struct SelectableLine {
    pub entry_idx: usize,
    pub line_in_entry: usize,
    pub screen_y: u16,
    pub screen_x: u16,
    pub width: u16,
    pub text: String,
}

impl SelectableLine {
    /// Column distance + clamped col within this line's span.
    fn col_metrics(&self, col: u16) -> Option<(u16, u16)> {
        if self.width == 0 {
            return None;
        }
        let start = self.screen_x;
        let end = self.screen_x.saturating_add(self.width);
        Some(if col < start {
            (start.saturating_sub(col), 0)
        } else if col >= end {
            (
                col.saturating_sub(end.saturating_sub(1)),
                self.width.saturating_sub(1),
            )
        } else {
            (0, col.saturating_sub(start))
        })
    }

    #[must_use]
    pub fn display_width(text: &str) -> u16 {
        u16::try_from(text.width()).unwrap_or(u16::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextHit {
    pub entry_idx: usize,
    pub line_in_entry: usize,
    pub col: u16,
}

impl TextHit {
    fn ordered(a: Self, b: Self) -> (Self, Self) {
        let ka = (a.entry_idx, a.line_in_entry, a.col);
        let kb = (b.entry_idx, b.line_in_entry, b.col);
        if ka <= kb {
            (a, b)
        } else {
            (b, a)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextHit,
    pub head: TextHit,
}

#[derive(Debug, Clone, Copy)]
struct PendingDrag {
    anchor: TextHit,
    start_col: u16,
    start_row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragAutoScrollState {
    pub direction: AutoScrollDirection,
    pub speed: u16,
}

/// Character selection controller for the scrollback pane.
#[derive(Debug, Default)]
pub struct Selection {
    pending: Option<PendingDrag>,
    active: Option<TextSelection>,
    lines: Vec<SelectableLine>,
    content_area: Rect,
    drag_pointer: Option<(u16, u16)>,
    auto_scroll: Option<DragAutoScrollState>,
}

const DRAG_THRESHOLD: u16 = 2;
const EDGE_THRESHOLD: u16 = 2;

impl Selection {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn active(&self) -> Option<TextSelection> {
        self.active
    }

    #[must_use]
    pub fn has_active(&self) -> bool {
        self.active.is_some()
    }

    #[must_use]
    pub fn auto_scroll(&self) -> Option<DragAutoScrollState> {
        self.auto_scroll
    }

    pub fn begin_frame(&mut self, content_area: Rect) {
        self.lines.clear();
        self.content_area = content_area;
    }

    pub fn register_line(&mut self, line: SelectableLine) {
        if line.width > 0 || !line.text.is_empty() {
            self.lines.push(line);
        }
    }

    pub fn clear(&mut self) {
        self.pending = None;
        self.active = None;
        self.drag_pointer = None;
        self.auto_scroll = None;
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.pending.is_some()
    }

    #[must_use]
    pub fn hit_test(&self, col: u16, row: u16) -> Option<TextHit> {
        let mut best: Option<(u16, TextHit)> = None;
        for line in &self.lines {
            if line.screen_y != row {
                continue;
            }
            let Some((dist, col_within)) = line.col_metrics(col) else {
                continue;
            };
            let hit = TextHit {
                entry_idx: line.entry_idx,
                line_in_entry: line.line_in_entry,
                col: col_within,
            };
            if dist == 0 {
                return Some(hit);
            }
            if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                best = Some((dist, hit));
            }
        }
        best.map(|(_, h)| h)
    }

    #[must_use]
    pub fn hit_test_drag_head(&self, anchor: TextHit, col: u16, row: u16) -> Option<TextHit> {
        let mut best: Option<((u16, u16), TextHit)> = None;
        for line in &self.lines {
            if line.entry_idx != anchor.entry_idx {
                continue;
            }
            let Some((col_dist, col_within)) = line.col_metrics(col) else {
                continue;
            };
            let key = (line.screen_y.abs_diff(row), col_dist);
            let hit = TextHit {
                entry_idx: line.entry_idx,
                line_in_entry: line.line_in_entry,
                col: col_within,
            };
            if best.as_ref().is_none_or(|(k, _)| key < *k) {
                best = Some((key, hit));
            }
        }
        best.map(|(_, h)| h).or_else(|| self.hit_test(col, row))
    }

    pub fn on_mouse_down(&mut self, col: u16, row: u16) {
        let Some(hit) = self.hit_test(col, row) else {
            self.clear();
            return;
        };
        self.pending = Some(PendingDrag {
            anchor: hit,
            start_col: col,
            start_row: row,
        });
        self.drag_pointer = Some((col, row));
        self.auto_scroll = None;
        self.active = Some(TextSelection {
            anchor: hit,
            head: hit,
        });
    }

    pub fn on_mouse_drag(&mut self, col: u16, row: u16) {
        let Some(pending) = self.pending else {
            return;
        };
        self.drag_pointer = Some((col, row));
        self.auto_scroll = self.compute_autoscroll(row);
        let moved = pending.start_col.abs_diff(col) >= DRAG_THRESHOLD
            || pending.start_row.abs_diff(row) >= DRAG_THRESHOLD;
        if !moved {
            return;
        }
        let head = self
            .hit_test_drag_head(pending.anchor, col, row)
            .unwrap_or(pending.anchor);
        self.active = Some(TextSelection {
            anchor: pending.anchor,
            head,
        });
    }

    pub fn refresh_drag_head_after_scroll(&mut self) {
        let Some(pending) = self.pending else {
            return;
        };
        let Some((col, row)) = self.drag_pointer else {
            return;
        };
        let head = self
            .hit_test_drag_head(pending.anchor, col, row)
            .unwrap_or(pending.anchor);
        self.active = Some(TextSelection {
            anchor: pending.anchor,
            head,
        });
    }

    /// Ends drag; returns selection if non-empty.
    pub fn on_mouse_up(&mut self) -> Option<TextSelection> {
        self.pending = None;
        self.drag_pointer = None;
        self.auto_scroll = None;
        let sel = self.active?;
        if sel.anchor == sel.head {
            return None;
        }
        Some(sel)
    }

    pub fn double_click_word(&mut self, col: u16, row: u16) {
        let Some(hit) = self.hit_test(col, row) else {
            return;
        };
        let Some(line) = self.find_line(hit) else {
            return;
        };
        let (start, end) = self.word_display_range(&line.text, hit.col);
        if start >= end {
            return;
        }
        self.active = Some(TextSelection {
            anchor: TextHit {
                entry_idx: hit.entry_idx,
                line_in_entry: hit.line_in_entry,
                col: start,
            },
            head: TextHit {
                entry_idx: hit.entry_idx,
                line_in_entry: hit.line_in_entry,
                col: end.saturating_sub(1).max(start),
            },
        });
        self.pending = None;
    }

    pub fn triple_click_line(&mut self, col: u16, row: u16) {
        let Some(hit) = self.hit_test(col, row) else {
            return;
        };
        let Some(line) = self.find_line(hit) else {
            return;
        };
        if line.width == 0 {
            return;
        }
        self.active = Some(TextSelection {
            anchor: TextHit {
                entry_idx: hit.entry_idx,
                line_in_entry: hit.line_in_entry,
                col: 0,
            },
            head: TextHit {
                entry_idx: hit.entry_idx,
                line_in_entry: hit.line_in_entry,
                col: line.width.saturating_sub(1),
            },
        });
        self.pending = None;
    }

    /// Reconstruct copy text by asking `source` for full entry lines.
    #[must_use]
    pub fn reconstruct_text(&self, source: &dyn LineSource) -> Option<String> {
        let sel = self.active?;
        let (a, b) = TextHit::ordered(sel.anchor, sel.head);
        if a == b {
            return None;
        }

        let mut out = String::new();
        let mut first = true;

        for entry_idx in a.entry_idx..=b.entry_idx {
            let lines = source.lines_of(entry_idx);
            if lines.is_empty() {
                continue;
            }
            let line_lo = if entry_idx == a.entry_idx {
                a.line_in_entry
            } else {
                0
            };
            let line_hi = if entry_idx == b.entry_idx {
                b.line_in_entry.min(lines.len().saturating_sub(1))
            } else {
                lines.len().saturating_sub(1)
            };
            for li in line_lo..=line_hi {
                let text = lines.get(li).map(String::as_str).unwrap_or("");
                let width = SelectableLine::display_width(text);
                let c0 = if entry_idx == a.entry_idx && li == a.line_in_entry {
                    a.col
                } else {
                    0
                };
                let c1 = if entry_idx == b.entry_idx && li == b.line_in_entry {
                    b.col.saturating_add(1).min(width)
                } else {
                    width
                };
                let slice = self.slice_by_display_cols(text, c0, c1);
                if !first {
                    out.push('\n');
                }
                first = false;
                out.push_str(&slice);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Paint selection highlight onto the buffer.
    pub fn paint_highlight(&self, buf: &mut Buffer) {
        let Some(sel) = self.active else {
            return;
        };
        let (a, b) = TextHit::ordered(sel.anchor, sel.head);
        let style = Style::default()
            .bg(Color::Rgb(40, 90, 140))
            .add_modifier(Modifier::BOLD);

        for line in &self.lines {
            let key = (line.entry_idx, line.line_in_entry);
            let ka = (a.entry_idx, a.line_in_entry);
            let kb = (b.entry_idx, b.line_in_entry);
            if key < ka || key > kb {
                continue;
            }
            let (c0, c1) = self.cols_on_line(line, a, b);
            if c0 >= c1 {
                continue;
            }
            let x0 = line.screen_x.saturating_add(c0);
            let x1 = line.screen_x.saturating_add(c1);
            for x in x0..x1 {
                if let Some(cell) = buf.cell_mut((x, line.screen_y)) {
                    cell.set_style(style);
                }
            }
        }
    }

    fn find_line(&self, hit: TextHit) -> Option<&SelectableLine> {
        self.lines.iter().find(|l| {
            l.entry_idx == hit.entry_idx && l.line_in_entry == hit.line_in_entry
        })
    }

    fn compute_autoscroll(&self, mouse_row: u16) -> Option<DragAutoScrollState> {
        let content_area = self.content_area;
        let top = content_area.y;
        let bottom = content_area.y.saturating_add(content_area.height);
        if content_area.height == 0 {
            return None;
        }
        if mouse_row < top.saturating_add(EDGE_THRESHOLD) {
            let distance = top.saturating_add(EDGE_THRESHOLD).saturating_sub(mouse_row);
            Some(DragAutoScrollState {
                direction: AutoScrollDirection::Up,
                speed: Self::speed_for_distance(distance),
            })
        } else if mouse_row >= bottom.saturating_sub(EDGE_THRESHOLD) {
            let distance = mouse_row
                .saturating_sub(bottom.saturating_sub(EDGE_THRESHOLD))
                .saturating_add(1);
            Some(DragAutoScrollState {
                direction: AutoScrollDirection::Down,
                speed: Self::speed_for_distance(distance),
            })
        } else {
            None
        }
    }

    fn speed_for_distance(distance: u16) -> u16 {
        match distance {
            0..=2 => 1,
            3..=5 => 2,
            6..=10 => 3,
            _ => 5,
        }
    }

    fn cols_on_line(&self, line: &SelectableLine, a: TextHit, b: TextHit) -> (u16, u16) {
        let key = (line.entry_idx, line.line_in_entry);
        let ka = (a.entry_idx, a.line_in_entry);
        let kb = (b.entry_idx, b.line_in_entry);
        let end = line.width;
        if key == ka && key == kb {
            let lo = min(a.col, b.col);
            let hi = max(a.col, b.col).saturating_add(1).min(end);
            (lo, hi)
        } else if key == ka {
            (a.col, end)
        } else if key == kb {
            (0, b.col.saturating_add(1).min(end))
        } else {
            (0, end)
        }
    }

    fn slice_by_display_cols(&self, text: &str, col0: u16, col1: u16) -> String {
        let col0 = col0 as usize;
        let col1 = col1 as usize;
        let mut out = String::new();
        let mut col = 0usize;
        for g in text.graphemes(true) {
            let w = g.width();
            let next = col.saturating_add(w);
            if next > col0 && col < col1 {
                out.push_str(g);
            }
            col = next;
            if col >= col1 {
                break;
            }
        }
        out
    }

    fn word_display_range(&self, text: &str, col: u16) -> (u16, u16) {
        let col = col as usize;
        let mut spans: Vec<(usize, usize, &str)> = Vec::new();
        let mut c = 0usize;
        for g in text.graphemes(true) {
            let w = g.width().max(1);
            spans.push((c, c + w, g));
            c += w;
        }
        if spans.is_empty() {
            return (0, 0);
        }
        let idx = spans
            .iter()
            .position(|(s, e, _)| col >= *s && col < *e)
            .unwrap_or(spans.len() - 1);
        let g = spans[idx].2;
        let is_cjk_or_wide = g.chars().next().is_some_and(|ch| {
            let u = ch as u32;
            (0x4E00..=0x9FFF).contains(&u)
                || (0x3400..=0x4DBF).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0x3000..=0x303F).contains(&u)
                || (0xFF00..=0xFFEF).contains(&u)
                || g.width() >= 2
        });
        if is_cjk_or_wide {
            return (spans[idx].0 as u16, spans[idx].1 as u16);
        }
        let is_word = |s: &str| {
            s.chars()
                .next()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        };
        if !is_word(g) {
            return (spans[idx].0 as u16, spans[idx].1 as u16);
        }
        let mut lo = idx;
        let mut hi = idx;
        while lo > 0 && is_word(spans[lo - 1].2) {
            lo -= 1;
        }
        while hi + 1 < spans.len() && is_word(spans[hi + 1].2) {
            hi += 1;
        }
        (spans[lo].0 as u16, spans[hi].1 as u16)
    }
}
