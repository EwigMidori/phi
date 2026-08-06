//! Paste policy object: short text inline, large/multi-line → chip.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use xai_ratatui_textarea::{ElementKind, TextArea};

/// Grok-aligned paste rules for the prompt TextArea.
#[derive(Debug, Clone)]
pub struct PastePolicy {
    kind_paste: ElementKind,
    /// Chip when line count ≥ this (non-compact Grok default: 4).
    min_lines: usize,
    /// Chip when byte length exceeds this (Grok: 10_000).
    max_inline_bytes: usize,
}

impl Default for PastePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl PastePolicy {
    pub const KIND_PASTE: ElementKind = ElementKind(1);

    #[must_use]
    pub fn new() -> Self {
        Self {
            kind_paste: Self::KIND_PASTE,
            min_lines: 4,
            max_inline_bytes: 10_000,
        }
    }

    /// Insert `raw` into `textarea` under this policy.
    pub fn apply(&self, textarea: &mut TextArea, raw: &str) {
        if raw.is_empty() {
            return;
        }
        let normalized = self.normalize_cr(raw);
        let line_count = normalized.lines().count();
        let by_lines = line_count >= self.min_lines;
        let by_bytes = normalized.len() > self.max_inline_bytes;
        if by_lines || by_bytes {
            let label = if by_bytes {
                self.size_label(normalized.len())
            } else {
                format!(
                    "Pasted: {line_count} line{}",
                    if line_count == 1 { "" } else { "s" }
                )
            };
            let display = self.chip_line(label);
            let _ = textarea.insert_element(&normalized, self.kind_paste, Some(display));
        } else {
            textarea.insert_str(&normalized);
        }
    }

    fn chip_line(&self, label: String) -> Line<'static> {
        let bg = Color::Rgb(40, 40, 50);
        Line::from(vec![
            Span::styled("[", Style::default().fg(Color::DarkGray).bg(bg)),
            Span::styled(label, Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg)),
            Span::styled("]", Style::default().fg(Color::DarkGray).bg(bg)),
        ])
    }

    /// Bare `\r` → `\n`; leave `\r\n` intact.
    fn normalize_cr(&self, text: &str) -> String {
        let mut s = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\r' && chars.peek() != Some(&'\n') {
                s.push('\n');
            } else {
                s.push(c);
            }
        }
        s
    }

    fn size_label(&self, byte_len: usize) -> String {
        if byte_len >= 1_000_000 {
            format!("Pasted: {:.1} MB", byte_len as f64 / 1_000_000.0)
        } else if byte_len >= 1000 {
            format!("Pasted: {} KB", byte_len / 1000)
        } else {
            format!("Pasted: {byte_len} bytes")
        }
    }
}
