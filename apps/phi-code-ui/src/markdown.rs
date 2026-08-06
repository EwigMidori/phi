//! Product markdown renderer object.
//!
//! Owns pretty-mode style (HIDDEN outers). Height, paint, and plain-text for
//! copy all ask this object so they cannot diverge.

use anstyle::Style;
use ratatui::text::Line;
use xai_grok_markdown::{render_markdown_ratatui, MarkdownStyle, StreamingMarkdownRenderer};

/// Pretty markdown collaborator shared by scrollback layout and the painter.
#[derive(Debug, Clone)]
pub struct ProductMarkdown {
    style: MarkdownStyle,
}

impl Default for ProductMarkdown {
    fn default() -> Self {
        Self::new()
    }
}

impl ProductMarkdown {
    /// Build the product pretty style (outer markers hidden).
    #[must_use]
    pub fn new() -> Self {
        Self {
            style: MarkdownStyle {
                heading_inner: [Style::new().bold(); 6],
                heading_outer: [Style::new().dimmed().hidden(); 6],
                strong_inner: Style::new().bold(),
                strong_outer: Style::new().dimmed().hidden(),
                emphasis_inner: Style::new().italic(),
                emphasis_outer: Style::new().dimmed().hidden(),
                strikethrough_inner: Style::new().strikethrough(),
                strikethrough_outer: Style::new().dimmed().hidden(),
                inline_code_inner: Style::new().bold(),
                inline_code_outer: Style::new().dimmed().hidden(),
                blockquote_outer: Style::new().dimmed(),
                task_checked: Style::new(),
                task_unchecked: Style::new().dimmed(),
                list_item: Style::new().dimmed(),
                rule: Style::new(),
                link_outer: Style::new(),
                link_text: Style::new().bold(),
                link_url: Style::new().dimmed(),
                link_title: Style::new(),
                code_outer: Style::new().dimmed().hidden(),
                code_language: Style::new().hidden(),
                code_untagged: Style::new(),
                code_background: Style::new(),
                table_outer: Style::new().bold(),
                text: Style::new(),
                math: Style::new().italic(),
            },
        }
    }

    /// Style message for streaming renderer construction.
    #[must_use]
    pub fn style(&self) -> MarkdownStyle {
        self.style
    }

    /// Styled pretty lines for painting.
    #[must_use]
    pub fn pretty_lines(&self, content: &str) -> Vec<Line<'static>> {
        if content.is_empty() {
            return vec![Line::from("")];
        }
        render_markdown_ratatui(content, self.style, true, None).0
    }

    /// Plain text of pretty lines (selection / copy geometry).
    #[must_use]
    pub fn plain_lines(&self, content: &str) -> Vec<String> {
        let lines = self.pretty_lines(content);
        let mut plains: Vec<String> = lines.iter().map(Self::plain_of_line).collect();
        if plains.is_empty() {
            plains.push(String::new());
        }
        plains
    }

    /// Display height in terminal rows.
    #[must_use]
    pub fn height(&self, content: &str) -> usize {
        if content.is_empty() {
            return 1;
        }
        self.pretty_lines(content).len().max(1)
    }

    /// Spawn a streaming renderer bound to this style.
    #[must_use]
    pub fn streaming_renderer(&self) -> StreamingMarkdownRenderer {
        StreamingMarkdownRenderer::new(self.style, true)
    }

    #[must_use]
    pub fn plain_of_line(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_hides_heading_markers() {
        let md = ProductMarkdown::new();
        let joined = md.plain_lines("### Title\n\nbody").join("\n");
        assert!(
            !joined.contains("###"),
            "pretty plain must hide outer markers: {joined:?}"
        );
    }

    #[test]
    fn height_matches_plain_line_count() {
        let md = ProductMarkdown::new();
        let content = "# T\n\n- a\n- b\n\n```\ncode\n```\n";
        assert_eq!(md.height(content), md.plain_lines(content).len().max(1));
    }
}
