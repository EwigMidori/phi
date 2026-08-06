//! Horizontal column layout for scrollback entries.
//!
//! Grok-aligned: accent (1) | pad (2) | content | pad (2).

use ratatui::layout::{Constraint, Layout, Rect};

/// Column geometry for one entry row area.
#[derive(Debug, Clone, Copy)]
pub struct HorizontalLayout {
    accent: Rect,
    left_pad: Rect,
    content: Rect,
    right_pad: Rect,
}

impl HorizontalLayout {
    pub const ACCENT: u16 = 1;
    pub const PAD_LEFT: u16 = 2;
    pub const PAD_RIGHT: u16 = 2;

    /// Partition `area` into accent / pads / content.
    #[must_use]
    pub fn for_area(area: Rect) -> Self {
        let [accent, left_pad, content, right_pad] = Layout::horizontal([
            Constraint::Length(Self::ACCENT),
            Constraint::Length(Self::PAD_LEFT),
            Constraint::Min(1),
            Constraint::Length(Self::PAD_RIGHT),
        ])
        .areas(area);
        Self {
            accent,
            left_pad,
            content,
            right_pad,
        }
    }

    /// Content column width given full entry area width.
    #[must_use]
    pub fn content_width_for(area_width: u16) -> u16 {
        area_width
            .saturating_sub(Self::ACCENT)
            .saturating_sub(Self::PAD_LEFT)
            .saturating_sub(Self::PAD_RIGHT)
            .max(1)
    }

    #[must_use]
    pub fn accent(&self) -> Rect {
        self.accent
    }

    #[must_use]
    pub fn left_pad(&self) -> Rect {
        self.left_pad
    }

    #[must_use]
    pub fn content(&self) -> Rect {
        self.content
    }

    #[must_use]
    pub fn right_pad(&self) -> Rect {
        self.right_pad
    }
}
