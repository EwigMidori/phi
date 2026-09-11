//! Scrollback **view** over durable kernel history + optional live generation overlay.
//!
//! - **Durable SoT:** [`TurnItem`] rows from [`phi_kernel::Transcript`] (via `set_durable`).
//! - **Live overlay:** in-flight reasoning/assistant text from bus deltas (not a second history).
//! - **Thinking chrome:** [`ThinkingPresenter`] — stable **open** span while streaming;
//!   **sealed** by frozen content key only after flush (preserves expand across text).

use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::time::Instant;

use phi_kernel::{ToolResultStatus, TurnItem};
use textwrap::{Options, wrap};

use crate::markdown::ProductMarkdown;
use crate::scrollbar::ScrollInfo;
use crate::selection::LineSource;

/// Left accent semantics (painter maps to color).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    User,
    Assistant,
    Thinking,
    ToolRunning,
    ToolOk,
    ToolError,
    ToolOther,
}

/// Layout of a [`TurnItem::Reasoning`] entry for paint / hit-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingLayout {
    pub header_rows: usize,
    pub body_rows: usize,
    pub expanded: bool,
    pub streaming: bool,
}

impl ThinkingLayout {
    #[must_use]
    pub fn total_rows(self) -> usize {
        self.header_rows.saturating_add(self.body_rows)
    }

    #[must_use]
    pub fn is_header_row(self, line_in_entry: usize) -> bool {
        self.header_rows > 0 && line_in_entry == 0
    }
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

/// In-flight generation (assistant body only; CoT lives in [`ThinkingPresenter::open`]).
#[derive(Debug, Clone, Default)]
struct LiveOverlay {
    assistant: String,
}

/// Conversation scrollback view.
#[derive(Debug, Clone)]
pub struct Scrollback {
    /// Durable transcript projection.
    durable: Vec<TurnItem>,
    /// Merged display list (durable + live tails).
    items: Vec<TurnItem>,
    live: Option<LiveOverlay>,
    folded: HashSet<usize>,
    selected: Option<usize>,
    scroll_offset: usize,
    stick_bottom: bool,
    layout_width: u16,
    entry_heights: Vec<usize>,
    virtual_y: Vec<usize>,
    total_height: usize,
    markdown: ProductMarkdown,
    thinking: ThinkingPresenter,
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
            durable: Vec::new(),
            items: Vec::new(),
            live: None,
            folded: HashSet::new(),
            selected: None,
            scroll_offset: 0,
            stick_bottom: true,
            layout_width: 0,
            entry_heights: Vec::new(),
            virtual_y: Vec::new(),
            total_height: 0,
            markdown: ProductMarkdown::new(),
            thinking: ThinkingPresenter::new(),
        }
    }

    #[must_use]
    pub fn markdown(&self) -> &ProductMarkdown {
        &self.markdown
    }

    /// Display items (durable + live overlay).
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

    /// Index of the live assistant row in the display list, if any.
    #[must_use]
    pub fn streaming_index(&self) -> Option<usize> {
        let live = self.live.as_ref()?;
        if live.assistant.is_empty() {
            return None;
        }
        // Live assistant is always last when present.
        Some(self.items.len().saturating_sub(1))
    }

    #[must_use]
    pub fn is_streaming(&self) -> bool {
        self.live.is_some()
    }

    #[must_use]
    pub fn is_thinking(&self) -> bool {
        self.thinking.is_open_streaming()
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

    /// Replace durable history from transcript (SoT). Live overlay kept until `end_live`.
    pub fn set_durable(&mut self, items: Vec<TurnItem>) {
        self.durable = items;
        self.rematerialize();
    }

    /// Open a live generation overlay (after user is already in durable history).
    pub fn begin_live(&mut self) {
        self.live = Some(LiveOverlay::default());
        self.rematerialize();
        self.scroll_to_bottom();
    }

    pub fn live_reasoning_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let _ = self.live.get_or_insert_with(LiveOverlay::default);
        self.thinking.on_reasoning_delta(delta);
        self.rematerialize();
        self.scroll_to_bottom();
    }

    /// Append answer text. Returns `true` when live CoT was sealed (kernel has
    /// flushed reasoning to the transcript — caller should `set_durable`).
    pub fn live_text_delta(&mut self, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let live = self.live.get_or_insert_with(LiveOverlay::default);
        // Seal open CoT into presenter (preserves expanded) before dropping live row.
        let reasoning_flushed = self.thinking.seal_open();
        live.assistant.push_str(delta);
        self.rematerialize();
        self.scroll_to_bottom();
        reasoning_flushed
    }

    /// End live overlay (Done / Error / Stopped). Caller should `set_durable` after.
    pub fn end_live(&mut self) {
        let _ = self.thinking.seal_open();
        self.live = None;
        self.rematerialize();
        self.scroll_to_bottom();
    }

    /// Durable-only helpers for tests / offline paint.
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.durable.push(TurnItem::User {
            content: phi_kernel::MessageContent::text(content),
        });
        self.rematerialize();
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.durable.push(TurnItem::Assistant {
            content: content.into(),
        });
        self.rematerialize();
    }

    pub fn toggle_thinking(&mut self, entry_idx: usize) -> bool {
        let Some(TurnItem::Reasoning { content }) = self.items.get(entry_idx) else {
            return false;
        };
        if !self.thinking.toggle_for_content(content) {
            return false;
        }
        self.invalidate_layout();
        true
    }

    #[must_use]
    pub fn is_thinking_header(&self, entry_idx: usize, line_in_entry: usize) -> bool {
        if self.folded.contains(&entry_idx) {
            return false;
        }
        self.thinking_layout(entry_idx, self.layout_width.max(1) as usize)
            .is_some_and(|lay| lay.is_header_row(line_in_entry))
    }

    #[must_use]
    pub fn thinking_layout(&self, entry_idx: usize, width: usize) -> Option<ThinkingLayout> {
        let TurnItem::Reasoning { content } = self.items.get(entry_idx)? else {
            return None;
        };
        if self.folded.contains(&entry_idx) {
            return None;
        }
        let chrome = self.thinking.chrome_for(content);
        let expanded = chrome.is_some_and(|c| c.expanded);
        let streaming = chrome.is_some_and(|c| c.streaming);
        let body_rows = if expanded {
            EntryView::wrap_text(content, width.max(1)).len()
        } else {
            0
        };
        Some(ThinkingLayout {
            header_rows: 1,
            body_rows,
            expanded,
            streaming,
        })
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

    #[must_use]
    pub fn selected_plain_text(&self) -> Option<String> {
        let i = self.selected?;
        let item = self.items.get(i)?;
        Some(match item {
            TurnItem::Continuation { .. } | TurnItem::ModelResponse { .. } => return None,
            TurnItem::User { content } => content.plain_text(),
            TurnItem::Assistant { content } | TurnItem::Reasoning { content } => content.clone(),
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

    #[must_use]
    pub fn is_following(&self) -> bool {
        self.stick_bottom
    }

    #[must_use]
    pub fn scroll_info(&self, viewport_h: usize) -> ScrollInfo {
        ScrollInfo {
            offset: self.scroll_offset,
            viewport_h: u16::try_from(viewport_h.max(1)).unwrap_or(u16::MAX),
            total_h: self.total_height,
            following: self.stick_bottom,
        }
    }

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
        let stream_i = self.streaming_index();

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
            let view = EntryView::new(
                &self.items[i],
                self.folded.contains(&i),
                &self.markdown,
                self.chrome_for_item(&self.items[i]),
                stream_i == Some(i),
            );
            out.push(VisibleSegment {
                entry_index: i,
                item: self.items[i].clone(),
                folded: self.folded.contains(&i),
                selected: self.selected == Some(i),
                streaming: stream_i == Some(i),
                accent: view.accent(),
                clip_top,
                visible_rows,
            });
        }
        out
    }

    #[must_use]
    pub fn entry_lines(&self, index: usize) -> Vec<String> {
        let width = self.layout_width.max(1) as usize;
        self.entry_display_lines(index, width)
    }

    fn rematerialize(&mut self) {
        let mut items = self.durable.clone();
        // Open CoT span (stable chrome) appears as a live reasoning row until sealed.
        if let Some(buf) = self.thinking.open_buffer() {
            let already = items
                .iter()
                .any(|it| matches!(it, TurnItem::Reasoning { content } if content == buf));
            if !already {
                items.push(TurnItem::Reasoning {
                    content: buf.to_owned(),
                });
            }
        }
        if let Some(live) = &self.live {
            if !live.assistant.is_empty() {
                items.push(TurnItem::Assistant {
                    content: live.assistant.clone(),
                });
            }
        }
        self.items = items;
        self.folded.retain(|&i| i < self.items.len());
        if let Some(s) = self.selected
            && s >= self.items.len()
        {
            self.selected = None;
        }
        self.invalidate_layout();
    }

    fn chrome_for_item(&self, item: &TurnItem) -> Option<&ThinkingChrome> {
        match item {
            TurnItem::Reasoning { content } => self.thinking.chrome_for(content),
            _ => None,
        }
    }

    fn entry_display_lines(&self, index: usize, width: usize) -> Vec<String> {
        let Some(item) = self.items.get(index) else {
            return Vec::new();
        };
        EntryView::new(
            item,
            self.folded.contains(&index),
            &self.markdown,
            self.chrome_for_item(item),
            self.streaming_index() == Some(index),
        )
        .display_lines(width)
    }

    fn entry_height(&self, index: usize, width: usize) -> usize {
        self.entry_display_lines(index, width).len().max(1)
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
        for i in 0..self.items.len() {
            self.virtual_y.push(y);
            let h = self.entry_height(i, w);
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

fn content_key(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ── ThinkingPresenter: open slot + sealed-by-frozen-content ────────────────

/// UI presentation for reasoning rows.
///
/// - **open:** one stable span while CoT streams (expand survives more deltas).
/// - **sealed:** frozen content key after flush (expand survives assistant text).
#[derive(Debug, Clone, Default)]
struct ThinkingPresenter {
    open: Option<OpenThinkingSpan>,
    sealed: HashMap<u64, ThinkingChrome>,
}

#[derive(Debug, Clone)]
struct OpenThinkingSpan {
    buffer: String,
    chrome: ThinkingChrome,
}

impl ThinkingPresenter {
    fn new() -> Self {
        Self::default()
    }

    fn is_open_streaming(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|o| o.chrome.streaming && !o.buffer.is_empty())
    }

    fn open_buffer(&self) -> Option<&str> {
        self.open.as_ref().map(|o| o.buffer.as_str())
    }

    fn on_reasoning_delta(&mut self, delta: &str) {
        let span = self.open.get_or_insert_with(|| OpenThinkingSpan {
            buffer: String::new(),
            chrome: ThinkingChrome::new_streaming(),
        });
        span.buffer.push_str(delta);
        span.chrome.streaming = true;
    }

    /// Freeze open span into sealed map (preserves `expanded`). Returns true if sealed.
    fn seal_open(&mut self) -> bool {
        let Some(mut span) = self.open.take() else {
            return false;
        };
        if span.buffer.is_empty() {
            return false;
        }
        span.chrome.finish();
        let key = content_key(&span.buffer);
        self.sealed.insert(key, span.chrome);
        true
    }

    fn chrome_for(&self, content: &str) -> Option<&ThinkingChrome> {
        if let Some(open) = &self.open {
            if open.buffer == content {
                return Some(&open.chrome);
            }
        }
        self.sealed.get(&content_key(content))
    }

    fn chrome_for_mut(&mut self, content: &str) -> Option<&mut ThinkingChrome> {
        if let Some(open) = &mut self.open {
            if open.buffer == content {
                return Some(&mut open.chrome);
            }
        }
        self.sealed.get_mut(&content_key(content))
    }

    fn toggle_for_content(&mut self, content: &str) -> bool {
        if let Some(chrome) = self.chrome_for_mut(content) {
            chrome.expanded = !chrome.expanded;
            return true;
        }
        // Sealed row without chrome yet (e.g. loaded durable): create collapsed then expand.
        let key = content_key(content);
        let chrome = self
            .sealed
            .entry(key)
            .or_insert_with(ThinkingChrome::default_collapsed);
        chrome.expanded = !chrome.expanded;
        true
    }
}

#[derive(Debug, Clone)]
struct ThinkingChrome {
    expanded: bool,
    streaming: bool,
    started_at: Instant,
    finished_elapsed_ms: Option<u64>,
}

impl ThinkingChrome {
    fn new_streaming() -> Self {
        Self {
            expanded: false,
            streaming: true,
            started_at: Instant::now(),
            finished_elapsed_ms: None,
        }
    }

    fn default_collapsed() -> Self {
        Self {
            expanded: false,
            streaming: false,
            started_at: Instant::now(),
            finished_elapsed_ms: None,
        }
    }

    fn finish(&mut self) {
        self.streaming = false;
        if self.finished_elapsed_ms.is_none() {
            self.finished_elapsed_ms = Some(self.started_at.elapsed().as_millis() as u64);
        }
    }

    fn elapsed_ms(&self) -> Option<u64> {
        self.finished_elapsed_ms.or_else(|| {
            self.streaming
                .then(|| self.started_at.elapsed().as_millis() as u64)
        })
    }

    fn header_line(&self) -> String {
        let chevron = if self.expanded { "▾" } else { "▸" };
        if self.streaming {
            format!("{chevron} Thinking…")
        } else if let Some(ms) = self.elapsed_ms() {
            let secs = ms as f64 / 1000.0;
            if self.expanded {
                format!("{chevron} Thought for {secs:.1}s")
            } else {
                format!("{chevron} Thought for {secs:.1}s  (click to expand)")
            }
        } else if self.expanded {
            format!("{chevron} Thought")
        } else {
            format!("{chevron} Thought  (click to expand)")
        }
    }
}

struct EntryView<'a> {
    item: &'a TurnItem,
    folded: bool,
    markdown: &'a ProductMarkdown,
    thinking_chrome: Option<&'a ThinkingChrome>,
}

impl<'a> EntryView<'a> {
    fn new(
        item: &'a TurnItem,
        folded: bool,
        markdown: &'a ProductMarkdown,
        thinking_chrome: Option<&'a ThinkingChrome>,
        _is_streaming_target: bool,
    ) -> Self {
        Self {
            item,
            folded,
            markdown,
            thinking_chrome,
        }
    }

    fn accent(&self) -> Accent {
        match self.item {
            TurnItem::Continuation { .. } | TurnItem::ModelResponse { .. } => Accent::Thinking,
            TurnItem::User { .. } => Accent::User,
            TurnItem::Reasoning { .. } => Accent::Thinking,
            TurnItem::Assistant { .. } => Accent::Assistant,
            TurnItem::ToolCall { .. } => Accent::ToolRunning,
            TurnItem::ToolResult { status, .. } => match status {
                ToolResultStatus::Ok => Accent::ToolOk,
                ToolResultStatus::Error | ToolResultStatus::Incomplete => Accent::ToolError,
                ToolResultStatus::Denied | ToolResultStatus::Ask => Accent::ToolOther,
            },
        }
    }

    fn display_lines(&self, width: usize) -> Vec<String> {
        if self.folded {
            return vec![self.summary()];
        }
        let width = width.max(1);
        match self.item {
            TurnItem::Continuation { .. } | TurnItem::ModelResponse { .. } => Vec::new(),
            TurnItem::User { content } => {
                Self::wrap_text(&format!("› {}", content.plain_text()), width)
            }
            TurnItem::Reasoning { content } => {
                let chrome = self.thinking_chrome;
                let expanded = chrome.is_some_and(|c| c.expanded);
                // Always ask chrome for header (preserves ▾ when expanded while streaming).
                let header = chrome
                    .map(ThinkingChrome::header_line)
                    .unwrap_or_else(|| ThinkingChrome::default_collapsed().header_line());
                let mut lines = vec![header];
                if expanded {
                    lines.extend(Self::wrap_text(content, width));
                }
                lines
            }
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
            TurnItem::Continuation { .. } | TurnItem::ModelResponse { .. } => String::new(),
            TurnItem::User { content } => {
                let text = content.plain_text();
                let one = text.lines().next().unwrap_or("").trim();
                format!("› {one} …")
            }
            TurnItem::Reasoning { content } => {
                let one = content.lines().next().unwrap_or("").trim();
                format!("▸ Thought  {one} …")
            }
            TurnItem::Assistant { content } => {
                let one = content.lines().next().unwrap_or("").trim();
                format!("✦ {one} …")
            }
            TurnItem::ToolCall { tool_name, .. } => format!("⚙ {tool_name}  [folded]"),
            TurnItem::ToolResult {
                tool_name, status, ..
            } => format!("↳ {tool_name} [{}]  [folded]", Self::status_label(*status)),
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
        assert!(!sb.visible_segments(10).is_empty());
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
    fn live_stream_assistant_overlay() {
        let mut sb = Scrollback::new();
        sb.push_user("hi");
        sb.begin_live();
        assert!(!sb.live_text_delta("hel"));
        assert!(!sb.live_text_delta("lo"));
        assert!(sb.is_streaming());
        assert_eq!(sb.streaming_index(), Some(1));
        match &sb.items()[1] {
            TurnItem::Assistant { content } => assert_eq!(content, "hello"),
            _ => panic!("expected live assistant"),
        }
        sb.set_durable(vec![
            TurnItem::User {
                content: "hi".into(),
            },
            TurnItem::Assistant {
                content: "hello".into(),
            },
        ]);
        sb.end_live();
        assert!(!sb.is_streaming());
        assert_eq!(sb.items().len(), 2);
    }

    #[test]
    fn live_reasoning_then_text_clears_live_cot() {
        let mut sb = Scrollback::new();
        sb.begin_live();
        sb.live_reasoning_delta("secret cot");
        assert!(sb.live_text_delta("visible"));
        // Live CoT cleared once answer starts (caller reloads durable after true).
        assert!(
            sb.items()
                .iter()
                .any(|i| matches!(i, TurnItem::Assistant { content } if content == "visible"))
        );
        assert!(
            !sb.items()
                .iter()
                .any(|i| matches!(i, TurnItem::Reasoning { content } if content == "secret cot"))
        );
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
        assert_eq!(lines.len(), h);
        assert!(!lines.join("\n").contains("###"));
    }

    #[test]
    fn assistant_long_line_wraps_in_layout() {
        let long = format!("prefix {}", "x".repeat(200));
        let mut sb = Scrollback::new();
        sb.push_assistant(&long);
        sb.prepare(40, 80);
        assert!(sb.entry_lines(0).len() > 1);
    }

    #[test]
    fn thinking_toggle_by_content_key() {
        let mut sb = Scrollback::new();
        sb.set_durable(vec![TurnItem::Reasoning {
            content: "cot".into(),
        }]);
        sb.prepare(40, 20);
        assert!(sb.is_thinking_header(0, 0));
        assert!(sb.toggle_thinking(0));
        let lay = sb.thinking_layout(0, 40).unwrap();
        assert!(lay.expanded);
    }

    #[test]
    fn thinking_expand_survives_more_reasoning_and_text() {
        let mut sb = Scrollback::new();
        sb.begin_live();
        sb.live_reasoning_delta("step one");
        sb.prepare(40, 40);
        let idx = sb
            .items()
            .iter()
            .position(|i| matches!(i, TurnItem::Reasoning { .. }))
            .unwrap();
        assert!(sb.toggle_thinking(idx));
        assert!(sb.thinking_layout(idx, 40).unwrap().expanded);

        // More CoT must not reset expand (stable open span).
        sb.live_reasoning_delta("\nstep two");
        sb.prepare(40, 40);
        let idx = sb
            .items()
            .iter()
            .position(|i| matches!(i, TurnItem::Reasoning { .. }))
            .unwrap();
        assert!(
            sb.thinking_layout(idx, 40).unwrap().expanded,
            "expand must survive further reasoning deltas"
        );

        // Seal + durable + assistant text must keep expand.
        assert!(sb.live_text_delta("answer"));
        sb.set_durable(vec![
            TurnItem::Reasoning {
                content: "step one\nstep two".into(),
            },
            TurnItem::Assistant {
                content: "answer".into(),
            },
        ]);
        sb.prepare(40, 40);
        let idx = sb
            .items()
            .iter()
            .position(|i| matches!(i, TurnItem::Reasoning { .. }))
            .unwrap();
        assert!(
            sb.thinking_layout(idx, 40).unwrap().expanded,
            "expand must survive seal into durable + assistant stream"
        );
        // Header should still show expanded chevron, not forced ▸ Thinking…
        let lines = sb.entry_lines(idx);
        assert!(
            lines[0].starts_with('▾'),
            "header should stay expanded: {}",
            lines[0]
        );
    }
}
