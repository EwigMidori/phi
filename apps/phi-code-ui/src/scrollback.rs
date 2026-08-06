//! Scrollback view object over kernel [`TurnItem`].
//!
//! Collaborators:
//! - [`ProductMarkdown`] — assistant display height / plain lines
//! - [`EntryView`] — private: how one turn answers height / lines / accent

use std::collections::HashSet;

use phi_kernel::{ToolResultStatus, TurnItem};
use textwrap::{wrap, Options};

use crate::markdown::ProductMarkdown;
use crate::scrollbar::ScrollInfo;
use crate::selection::LineSource;

/// Left accent semantics (painter maps to color).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    User,
    Assistant,
    ToolRunning,
    ToolOk,
    ToolError,
    ToolOther,
}

/// One viewport-visible piece of an entry (may be a vertical clip).
#[derive(Debug, Clone)]
pub struct VisibleSegment {
    pub entry_index: usize,
    pub item: TurnItem,
    pub folded: bool,
    pub selected: bool,
    pub streaming: bool,
    pub accent: Accent,
    pub clip_top: usize,
    pub visible_rows: usize,
}

/// Conversation scrollback: facts + virtual layout + fold/selection/stream.
///
/// Private state; callers only send messages.
#[derive(Debug, Clone)]
pub struct Scrollback {
    items: Vec<TurnItem>,
    folded: HashSet<usize>,
    selected: Option<usize>,
    streaming: Option<usize>,
    scroll_offset: usize,
    stick_bottom: bool,
    layout_width: u16,
    entry_heights: Vec<usize>,
    virtual_y: Vec<usize>,
    total_height: usize,
    /// Shared pretty-md collaborator (height ≡ plain lines ≡ paint).
    markdown: ProductMarkdown,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::new()
    }
}

impl Scrollback {
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            folded: HashSet::new(),
            selected: None,
            streaming: None,
            scroll_offset: 0,
            stick_bottom: true,
            layout_width: 0,
            entry_heights: Vec::new(),
            virtual_y: Vec::new(),
            total_height: 0,
            markdown: ProductMarkdown::new(),
        }
    }

    #[must_use]
    pub fn markdown(&self) -> &ProductMarkdown {
        &self.markdown
    }

    #[must_use]
    pub fn items(&self) -> &[TurnItem] {
        &self.items
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[must_use]
    pub fn streaming_index(&self) -> Option<usize> {
        self.streaming
    }

    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.streaming.is_some()
    }

    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    #[must_use]
    pub fn total_height(&self) -> usize {
        self.total_height
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn set_items(&mut self, items: Vec<TurnItem>) {
        self.items = items;
        self.streaming = None;
        self.folded.retain(|&i| i < self.items.len());
        if let Some(s) = self.selected
            && s >= self.items.len()
        {
            self.selected = self.items.len().checked_sub(1);
        }
        self.invalidate_layout();
    }

    pub fn push(&mut self, item: TurnItem) {
        self.items.push(item);
        self.invalidate_layout();
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.push(TurnItem::User {
            content: content.into(),
        });
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.push(TurnItem::Assistant {
            content: content.into(),
        });
    }

    pub fn begin_assistant_stream(&mut self) {
        if self.streaming.is_some() {
            self.finish_assistant_stream();
        }
        self.push_assistant("");
        self.streaming = Some(self.items.len() - 1);
        self.scroll_to_bottom();
    }

    pub fn append_assistant_delta(&mut self, delta: &str) {
        let Some(i) = self.streaming else {
            return;
        };
        if let Some(TurnItem::Assistant { content }) = self.items.get_mut(i) {
            content.push_str(delta);
        }
        self.invalidate_layout();
    }

    pub fn finish_assistant_stream(&mut self) {
        self.streaming = None;
        self.invalidate_layout();
    }

    pub fn is_folded(&self, index: usize) -> bool {
        self.folded.contains(&index)
    }

    pub fn toggle_fold(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        if !self.folded.remove(&index) {
            self.folded.insert(index);
        }
        self.invalidate_layout();
    }

    pub fn toggle_fold_selected(&mut self) {
        if let Some(i) = self.selected {
            self.toggle_fold(i);
        }
    }

    pub fn select(&mut self, index: usize) {
        if index < self.items.len() {
            self.selected = Some(index);
        }
    }

    pub fn select_delta(&mut self, delta: isize, viewport_h: usize) {
        if self.items.is_empty() {
            self.selected = None;
            return;
        }
        let n = self.items.len() as isize;
        let cur = self.selected.unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, n - 1) as usize;
        self.selected = Some(next);
        self.ensure_entry_visible(next, viewport_h);
    }

    #[must_use]
    pub fn entry_at_content_y(&self, content_y: usize) -> Option<usize> {
        for (i, &y0) in self.virtual_y.iter().enumerate() {
            let y1 = y0 + self.entry_heights.get(i).copied().unwrap_or(0);
            if content_y >= y0 && content_y < y1 {
                return Some(i);
            }
        }
        None
    }

    /// Source plain text of the selected entry (raw content, not display).
    #[must_use]
    pub fn selected_plain_text(&self) -> Option<String> {
        let i = self.selected?;
        let item = self.items.get(i)?;
        Some(match item {
            TurnItem::User { content } | TurnItem::Assistant { content } => content.clone(),
            TurnItem::ToolCall {
                tool_name, input, ..
            } => format!("{tool_name}\n{input}"),
            TurnItem::ToolResult {
                tool_name,
                status,
                output,
                ..
            } => format!("{tool_name} [{status:?}]\n{output}"),
        })
    }

    /// Positive = toward older content (lower offset).
    pub fn scroll_by(&mut self, delta: isize, viewport_h: usize) {
        self.stick_bottom = false;
        let max = self.max_offset(viewport_h);
        if delta > 0 {
            self.scroll_offset = self.scroll_offset.saturating_sub(delta as usize);
        } else {
            self.scroll_offset = self
                .scroll_offset
                .saturating_add((-delta) as usize)
                .min(max);
        }
        if self.scroll_offset >= max {
            self.stick_bottom = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.stick_bottom = true;
    }

    /// Whether the viewport is locked to the bottom (follow mode).
    #[must_use]
    pub fn is_following(&self) -> bool {
        self.stick_bottom
    }

    /// Snapshot for the history scrollbar.
    #[must_use]
    pub fn scroll_info(&self, viewport_h: usize) -> ScrollInfo {
        ScrollInfo {
            offset: self.scroll_offset,
            viewport_h: u16::try_from(viewport_h.max(1)).unwrap_or(u16::MAX),
            total_h: self.total_height,
            following: self.stick_bottom,
        }
    }

    /// Jump to an absolute scroll offset (scrollbar click / drag).
    pub fn set_scroll_offset(&mut self, offset: usize, viewport_h: usize) {
        let max = self.max_offset(viewport_h);
        self.scroll_offset = offset.min(max);
        self.stick_bottom = self.scroll_offset >= max;
    }

    pub fn prepare(&mut self, content_width: u16, viewport_h: usize) {
        let w = content_width.max(1);
        if self.layout_width != w || self.entry_heights.len() != self.items.len() {
            self.rebuild_layout(w);
        }
        self.clamp_scroll(viewport_h);
        if self.stick_bottom {
            self.scroll_offset = self.max_offset(viewport_h);
        }
    }

    #[must_use]
    pub fn visible_segments(&self, viewport_h: usize) -> Vec<VisibleSegment> {
        if self.items.is_empty() || viewport_h == 0 || self.total_height == 0 {
            return Vec::new();
        }
        let view_top = self.scroll_offset;
        let view_bottom = view_top + viewport_h;
        let mut out = Vec::new();

        for (i, &y0) in self.virtual_y.iter().enumerate() {
            let h = self.entry_heights[i];
            let y1 = y0 + h;
            if y1 <= view_top || y0 >= view_bottom {
                continue;
            }
            let clip_top = view_top.saturating_sub(y0);
            let visible_rows = y1.min(view_bottom).saturating_sub(y0.max(view_top));
            if visible_rows == 0 {
                continue;
            }
            let view = EntryView::new(&self.items[i], self.folded.contains(&i), &self.markdown);
            out.push(VisibleSegment {
                entry_index: i,
                item: self.items[i].clone(),
                folded: self.folded.contains(&i),
                selected: self.selected == Some(i),
                streaming: self.streaming == Some(i),
                accent: view.accent(),
                clip_top,
                visible_rows,
            });
        }
        out
    }

    /// Display lines for an entry (matches painter plain text).
    #[must_use]
    pub fn entry_lines(&self, index: usize) -> Vec<String> {
        let width = self.layout_width.max(1) as usize;
        let Some(item) = self.items.get(index) else {
            return Vec::new();
        };
        EntryView::new(item, self.folded.contains(&index), &self.markdown).display_lines(width)
    }

    fn ensure_entry_visible(&mut self, index: usize, viewport_h: usize) {
        if index >= self.virtual_y.len() {
            return;
        }
        self.stick_bottom = false;
        let y0 = self.virtual_y[index];
        let y1 = y0 + self.entry_heights[index];
        let view_h = viewport_h.max(1);
        if y0 < self.scroll_offset {
            self.scroll_offset = y0;
        } else if y1 > self.scroll_offset + view_h {
            self.scroll_offset = y1.saturating_sub(view_h);
        }
        self.clamp_scroll(view_h);
    }

    fn invalidate_layout(&mut self) {
        self.layout_width = 0;
        self.entry_heights.clear();
        self.virtual_y.clear();
        self.total_height = 0;
    }

    fn rebuild_layout(&mut self, width: u16) {
        self.layout_width = width;
        self.entry_heights.clear();
        self.virtual_y.clear();
        let w = width as usize;
        let mut y = 0usize;
        for (i, item) in self.items.iter().enumerate() {
            self.virtual_y.push(y);
            let view = EntryView::new(item, self.folded.contains(&i), &self.markdown);
            let h = view.height(w);
            let gap = usize::from(i + 1 < self.items.len());
            self.entry_heights.push(h + gap);
            y += h + gap;
        }
        self.total_height = y;
    }

    fn max_offset(&self, viewport_h: usize) -> usize {
        self.total_height.saturating_sub(viewport_h.max(1))
    }

    fn clamp_scroll(&mut self, viewport_h: usize) {
        self.scroll_offset = self.scroll_offset.min(self.max_offset(viewport_h));
    }
}

impl LineSource for Scrollback {
    fn lines_of(&self, entry_idx: usize) -> Vec<String> {
        self.entry_lines(entry_idx)
    }
}

// ---------------------------------------------------------------------------
// EntryView — private collaborator: one turn presents itself
// ---------------------------------------------------------------------------

/// How a single [`TurnItem`] answers display questions.
struct EntryView<'a> {
    item: &'a TurnItem,
    folded: bool,
    markdown: &'a ProductMarkdown,
}

impl<'a> EntryView<'a> {
    fn new(item: &'a TurnItem, folded: bool, markdown: &'a ProductMarkdown) -> Self {
        Self {
            item,
            folded,
            markdown,
        }
    }

    fn accent(&self) -> Accent {
        match self.item {
            TurnItem::User { .. } => Accent::User,
            TurnItem::Assistant { .. } => Accent::Assistant,
            TurnItem::ToolCall { .. } => Accent::ToolRunning,
            TurnItem::ToolResult { status, .. } => match status {
                ToolResultStatus::Ok => Accent::ToolOk,
                ToolResultStatus::Error | ToolResultStatus::Incomplete => Accent::ToolError,
                ToolResultStatus::Denied | ToolResultStatus::Ask => Accent::ToolOther,
            },
        }
    }

    fn height(&self, width: usize) -> usize {
        if self.folded {
            return 1;
        }
        match self.item {
            TurnItem::Assistant { content } => self.markdown.height(content, width),
            _ => self.display_lines(width).len().max(1),
        }
    }

    fn display_lines(&self, width: usize) -> Vec<String> {
        if self.folded {
            return vec![self.summary()];
        }
        let width = width.max(1);
        match self.item {
            TurnItem::User { content } => Self::wrap_text(&format!("› {content}"), width),
            TurnItem::Assistant { content } => self.markdown.plain_lines(content, width),
            TurnItem::ToolCall {
                tool_name, input, ..
            } => {
                let mut lines = vec![format!("⚙ {tool_name}")];
                lines.extend(Self::wrap_text(&input.to_string(), width));
                lines
            }
            TurnItem::ToolResult {
                tool_name,
                status,
                output,
                ..
            } => {
                let mut lines = vec![format!("↳ {tool_name} [{}]", Self::status_label(*status))];
                lines.extend(Self::wrap_text(&output.to_string(), width));
                lines
            }
        }
    }

    fn summary(&self) -> String {
        match self.item {
            TurnItem::User { content } => {
                let one = content.lines().next().unwrap_or("").trim();
                format!("› {one} …")
            }
            TurnItem::Assistant { content } => {
                let one = content.lines().next().unwrap_or("").trim();
                format!("✦ {one} …")
            }
            TurnItem::ToolCall { tool_name, .. } => format!("⚙ {tool_name}  [folded]"),
            TurnItem::ToolResult {
                tool_name, status, ..
            } => format!(
                "↳ {tool_name} [{}]  [folded]",
                Self::status_label(*status)
            ),
        }
    }

    fn status_label(status: ToolResultStatus) -> &'static str {
        match status {
            ToolResultStatus::Ok => "ok",
            ToolResultStatus::Denied => "denied",
            ToolResultStatus::Error => "error",
            ToolResultStatus::Ask => "ask",
            ToolResultStatus::Incomplete => "incomplete",
        }
    }

    fn wrap_text(text: &str, width: usize) -> Vec<String> {
        let width = width.max(1);
        let opts = Options::new(width).break_words(true);
        let mut out = Vec::new();
        for paragraph in text.split('\n') {
            if paragraph.is_empty() {
                out.push(String::new());
                continue;
            }
            for line in wrap(paragraph, &opts) {
                out.push(line.into_owned());
            }
        }
        if out.is_empty() {
            out.push(String::new());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_layout_and_segments() {
        let mut sb = Scrollback::new();
        sb.push_user("hello");
        sb.push_assistant("world");
        sb.prepare(40, 10);
        assert!(sb.total_height() >= 2);
        let segs = sb.visible_segments(10);
        assert!(!segs.is_empty());
    }

    #[test]
    fn assistant_height_not_just_plain_wrap() {
        let md = "# Title\n\n- a\n- b\n\n```\ncode\n```\n";
        let mut sb = Scrollback::new();
        sb.push_assistant(md);
        sb.prepare(40, 40);
        assert!(sb.total_height() >= 3);
    }

    #[test]
    fn stream_appends_same_entry() {
        let mut sb = Scrollback::new();
        sb.begin_assistant_stream();
        sb.append_assistant_delta("hel");
        sb.append_assistant_delta("lo");
        assert_eq!(sb.items().len(), 1);
        assert_eq!(sb.streaming_index(), Some(0));
        match &sb.items()[0] {
            TurnItem::Assistant { content } => assert_eq!(content, "hello"),
            _ => panic!("expected assistant"),
        }
        sb.finish_assistant_stream();
        assert!(sb.streaming_index().is_none());
    }

    #[test]
    fn fold_collapses_height() {
        let mut sb = Scrollback::new();
        sb.push_assistant("a\nb\nc\nd\ne\nf\n");
        sb.prepare(40, 20);
        let tall = sb.total_height();
        sb.select(0);
        sb.toggle_fold_selected();
        sb.prepare(40, 20);
        assert!(sb.total_height() <= tall);
        assert_eq!(sb.entry_heights[0], 1);
    }

    #[test]
    fn assistant_entry_lines_match_md_height() {
        let md = "### Title\n\nHello **bold** and `code`.\n\n```rust\nfn x() {}\n```\n";
        let mut sb = Scrollback::new();
        sb.push_assistant(md);
        sb.prepare(60, 40);
        let lines = sb.entry_lines(0);
        let h = sb.markdown().height(md, 60);
        assert_eq!(
            lines.len(),
            h,
            "entry_lines must match painter/height line count"
        );
        let joined = lines.join("\n");
        assert!(
            !joined.contains("###"),
            "pretty plain lines should not expose raw heading markers: {joined:?}"
        );
    }

    #[test]
    fn assistant_long_line_wraps_in_layout() {
        let long = format!("prefix {}", "x".repeat(200));
        let mut sb = Scrollback::new();
        sb.push_assistant(&long);
        sb.prepare(40, 80);
        let lines = sb.entry_lines(0);
        assert!(
            lines.len() > 1,
            "assistant layout must wrap long content: {} lines",
            lines.len()
        );
        assert!(sb.total_height() > 1);
    }
}
