//! History scrollbar object (Grok-style: gap + track, follow-mode dim).
//!
//! Collaborates with [`crate::Scrollback`] via [`ScrollInfo`]; owns track
//! geometry and click/drag → offset mapping.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use tui_scrollbar::{ScrollLengths, ScrollMetrics, SUBCELL};

/// Snapshot of scroll position for paint + hit-test (from Scrollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollInfo {
    pub offset: usize,
    pub viewport_h: u16,
    pub total_h: usize,
    /// Stick-to-bottom / following live content.
    pub following: bool,
}

/// Result of mapping a track click/drag to a scroll offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarClick {
    Top,
    Bottom,
    Offset(usize),
}

/// History scrollbar: layout split, paint, click/drag → offset.
///
/// Grok layout: content | gap (1) | track (1) when content overflows.
#[derive(Debug, Default)]
pub struct HistoryScrollbar {
    /// Last painted track (1-col); used for hit testing.
    track: Option<Rect>,
    /// Thumb drag in progress.
    dragging: bool,
}

const GAP_COLS: u16 = 1;
const TRACK_COLS: u16 = 1;
const TOTAL_COLS: u16 = GAP_COLS + TRACK_COLS;

impl HistoryScrollbar {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether overflow requires a scrollbar.
    #[must_use]
    pub fn needed(total_h: usize, viewport_h: u16) -> bool {
        total_h > viewport_h as usize && viewport_h > 0
    }

    /// Split `area` into content + optional track (only when overflow).
    ///
    /// Returns `(content_area, track_area)`.
    #[must_use]
    pub fn split(area: Rect, total_h: usize) -> (Rect, Option<Rect>) {
        if !Self::needed(total_h, area.height) || area.width <= TOTAL_COLS {
            return (area, None);
        }
        let content_width = area.width.saturating_sub(TOTAL_COLS);
        let content = Rect {
            x: area.x,
            y: area.y,
            width: content_width,
            height: area.height,
        };
        let track = Rect {
            x: area.right().saturating_sub(TRACK_COLS),
            y: area.y,
            width: TRACK_COLS,
            height: area.height,
        };
        (content, Some(track))
    }

    /// Grab zone: track + 1-col slop left (Grok near-miss presses).
    #[must_use]
    pub fn grab_zone(track: Rect) -> Rect {
        let x = track.x.saturating_sub(GAP_COLS);
        Rect {
            x,
            y: track.y,
            width: (track.x - x)
                .saturating_add(track.width)
                .saturating_add(1),
            height: track.height,
        }
    }

    #[must_use]
    pub fn track(&self) -> Option<Rect> {
        self.track
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Paint track + thumb; remember track for later hit tests.
    pub fn paint(&mut self, buf: &mut Buffer, track: Option<Rect>, info: ScrollInfo) {
        self.track = track;
        let Some(track) = track else {
            return;
        };
        if track.width == 0 || track.height == 0 {
            return;
        }
        if !Self::needed(info.total_h, info.viewport_h) {
            return;
        }

        let (total_u16, offset_u16, scale) = Self::scale_for_u16(info.total_h, info.offset);
        let viewport = info.viewport_h;
        let lengths = ScrollLengths {
            content_len: total_u16 as usize,
            viewport_len: viewport as usize,
        };
        let metrics = ScrollMetrics::new(lengths, offset_u16 as usize, track.height);

        let (track_style, thumb_style) = Self::styles(info.following);
        let thumb_fill = match thumb_style.fg {
            Some(fg) => thumb_style.bg(fg),
            None => thumb_style,
        };

        for row in 0..track.height {
            let x = track.x;
            let y = track.y + row;
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            match metrics.cell_fill(row as usize) {
                tui_scrollbar::CellFill::Empty => {
                    cell.set_symbol(" ");
                    cell.set_style(track_style);
                }
                tui_scrollbar::CellFill::Full | tui_scrollbar::CellFill::Partial { .. } => {
                    // Full block; fill bg with thumb fg so macOS Terminal gaps vanish (Grok).
                    cell.set_symbol("\u{2588}");
                    cell.set_style(thumb_fill);
                }
            }
        }
        let _ = scale; // scaled lengths already applied
    }

    /// Mouse down on grab zone → jump/drag start; returns new scroll offset.
    pub fn on_mouse_down(
        &mut self,
        col: u16,
        row: u16,
        info: ScrollInfo,
    ) -> Option<usize> {
        let track = self.track?;
        let zone = Self::grab_zone(track);
        if !rect_contains(zone, col, row) {
            self.dragging = false;
            return None;
        }
        self.dragging = true;
        Some(self.offset_for_row(row, track, info))
    }

    /// Drag while holding thumb/track.
    pub fn on_mouse_drag(
        &mut self,
        col: u16,
        row: u16,
        info: ScrollInfo,
    ) -> Option<usize> {
        if !self.dragging {
            return None;
        }
        let track = self.track?;
        // Allow drag outside grab zone once started (Grok drag-off-track).
        let _ = col;
        Some(self.offset_for_row(row, track, info))
    }

    pub fn on_mouse_up(&mut self) {
        self.dragging = false;
    }

    /// Whether (col,row) is on the scrollbar grab zone.
    #[must_use]
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.track
            .map(|t| rect_contains(Self::grab_zone(t), col, row))
            .unwrap_or(false)
    }

    fn offset_for_row(&self, row: u16, track: Rect, info: ScrollInfo) -> usize {
        let cell = row.saturating_sub(track.y);
        match Self::click_to_offset(cell, track.height, info.total_h, info.viewport_h) {
            ScrollbarClick::Top => 0,
            ScrollbarClick::Bottom => info.total_h.saturating_sub(info.viewport_h as usize),
            ScrollbarClick::Offset(o) => o,
        }
    }

    /// Map track cell index → offset (Grok JumpToClick inverse of metrics).
    #[must_use]
    pub fn click_to_offset(
        cell_index: u16,
        track_cells: u16,
        total_h: usize,
        viewport_h: u16,
    ) -> ScrollbarClick {
        if track_cells == 0 {
            return ScrollbarClick::Top;
        }
        if cell_index == 0 {
            return ScrollbarClick::Top;
        }
        if cell_index >= track_cells.saturating_sub(1) {
            return ScrollbarClick::Bottom;
        }

        let (total_u16, _, scale) = Self::scale_for_u16(total_h, 0);
        let lengths = ScrollLengths {
            content_len: total_u16 as usize,
            viewport_len: viewport_h as usize,
        };
        let metrics = ScrollMetrics::new(lengths, 0, track_cells);
        let position = (cell_index as usize)
            .saturating_mul(SUBCELL)
            .saturating_add(SUBCELL / 2);
        let half_thumb = metrics.thumb_len() / 2;
        let thumb_start = position.saturating_sub(half_thumb);
        let scaled_offset = metrics.offset_for_thumb_start(thumb_start);
        ScrollbarClick::Offset(scaled_offset.saturating_mul(scale))
    }

    /// Scale tall content into u16 domain (Grok agent scrollbar).
    fn scale_for_u16(total_h: usize, offset: usize) -> (u16, u16, usize) {
        let scale = if total_h > u16::MAX as usize {
            (total_h / u16::MAX as usize) + 1
        } else {
            1
        };
        let total = (total_h / scale) as u16;
        let off = (offset / scale) as u16;
        (total, off, scale)
    }

    /// Follow → dim; scrolled-up → brighter (Grok follow-mode cue).
    fn styles(following: bool) -> (Style, Style) {
        if following {
            let track = Style::default().bg(Color::Rgb(28, 28, 32));
            let thumb = Style::default()
                .fg(Color::Rgb(60, 60, 70))
                .bg(Color::Rgb(28, 28, 32));
            (track, thumb)
        } else {
            let track = Style::default().bg(Color::Rgb(40, 40, 48));
            let thumb = Style::default()
                .fg(Color::Rgb(140, 140, 160))
                .bg(Color::Rgb(40, 40, 48));
            (track, thumb)
        }
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_reserves_when_overflow() {
        let area = Rect::new(0, 0, 40, 10);
        let (content, track) = HistoryScrollbar::split(area, 20);
        assert_eq!(content.width, 38);
        let t = track.expect("track");
        assert_eq!(t.x, 39);
        assert_eq!(t.width, 1);
    }

    #[test]
    fn split_full_width_when_fits() {
        let area = Rect::new(0, 0, 40, 10);
        let (content, track) = HistoryScrollbar::split(area, 5);
        assert_eq!(content.width, 40);
        assert!(track.is_none());
    }

    #[test]
    fn click_top_and_bottom() {
        assert_eq!(
            HistoryScrollbar::click_to_offset(0, 10, 100, 10),
            ScrollbarClick::Top
        );
        assert_eq!(
            HistoryScrollbar::click_to_offset(9, 10, 100, 10),
            ScrollbarClick::Bottom
        );
    }

    #[test]
    fn mid_click_is_proportional_offset() {
        match HistoryScrollbar::click_to_offset(5, 10, 100, 10) {
            ScrollbarClick::Offset(o) => {
                assert!(o > 0 && o < 90, "mid click offset was {o}");
            }
            other => panic!("expected Offset, got {other:?}"),
        }
    }
}
