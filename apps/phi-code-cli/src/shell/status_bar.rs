//! Status bar chrome (shell UI): summary row + optional expanded panel.
//!
//! Owns presentation of stream / model / usage / notes / multi-line tip from a
//! paint-time [`StatusSnapshot`]. String formatting of channel/usage snapshots
//! lives in [`super::status_format`]; token aggregation stays in `phi-code-core`.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

// ── Snapshot (value object assembled by the shell) ─────────────────────────

/// Generation activity for the summary row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Idle,
    Streaming,
    Thinking,
}

/// Typed status note (no string-prefix hacks for severity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusNote {
    pub kind: NoteKind,
    pub text: String,
}

/// One paint-time reading of product state (no live borrowing of panes).
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub stream: StreamKind,
    pub model_id: String,
    /// Compact usage for the bar; empty = omit segment.
    pub usage_bar: String,
    /// Only set when multi-line input is on; single-line omits the segment.
    pub multiline: bool,
    /// Expanded panel: channel lines (already human-readable).
    pub channel_lines: Vec<String>,
    /// Expanded panel: usage detail lines.
    pub usage_lines: Vec<String>,
    /// Expanded panel: layout summary (entries / rows).
    pub layout_line: String,
}

// ── StatusBar object ───────────────────────────────────────────────────────

/// Owns expand/collapse, sticky note, hit target; paints from a snapshot.
pub struct StatusBar {
    expanded: bool,
    hit: Rect,
    note: Option<StatusNote>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBar {
    #[must_use]
    pub fn new() -> Self {
        Self {
            expanded: false,
            hit: Rect::default(),
            note: None,
        }
    }

    /// Rows needed for the current expand state (for outer layout).
    #[must_use]
    pub fn height(&self) -> u16 {
        if self.expanded {
            1 + PANEL_BODY_ROWS
        } else {
            1
        }
    }

    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub fn set_note(&mut self, kind: NoteKind, text: impl Into<String>) {
        self.note = Some(StatusNote {
            kind,
            text: text.into(),
        });
    }

    pub fn clear_note(&mut self) {
        self.note = None;
    }

    #[must_use]
    pub fn note(&self) -> Option<&StatusNote> {
        self.note.as_ref()
    }

    /// True if (col, row) is on the status chrome (summary or open panel).
    #[must_use]
    pub fn contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.hit, col, row)
    }

    /// Paint and record hit target. Sticky note is owned by this bar.
    pub fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, snapshot: &StatusSnapshot) {
        self.hit = area;
        let summary = build_summary_line(self.expanded, snapshot, self.note.as_ref());

        if !self.expanded {
            frame.render_widget(Paragraph::new(summary), area);
            return;
        }

        let body = build_panel_lines(snapshot);
        let [head, panel] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(PANEL_BODY_ROWS),
        ])
        .areas(area);

        frame.render_widget(Paragraph::new(summary), head);
        frame.render_widget(
            Paragraph::new(body).block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
            panel,
        );
    }
}

/// Fixed panel body capacity (channel + usage + layout). Stable so layout does not thrash.
const PANEL_BODY_ROWS: u16 = 8;

// ── Summary segments ───────────────────────────────────────────────────────

struct Segment {
    text: String,
    style: Style,
    /// Chevron is glued to the next segment without a dot separator.
    glue_next: bool,
}

fn build_summary_line(
    expanded: bool,
    snap: &StatusSnapshot,
    note: Option<&StatusNote>,
) -> Line<'static> {
    let mut segs = Vec::new();
    segs.push(Segment {
        text: format!(" {} ", if expanded { "▾" } else { "▸" }),
        style: Style::default().fg(Color::DarkGray),
        glue_next: true,
    });
    segs.push(stream_segment(snap.stream));
    segs.push(Segment {
        text: snap.model_id.clone(),
        style: Style::default().fg(Color::Cyan),
        glue_next: false,
    });
    if !snap.usage_bar.is_empty() {
        segs.push(Segment {
            text: snap.usage_bar.clone(),
            style: Style::default().fg(Color::Yellow),
            glue_next: false,
        });
    }
    if let Some(n) = note {
        segs.push(Segment {
            text: truncate_chars(&n.text, 48),
            style: note_style(n.kind),
            glue_next: false,
        });
    }
    // Single-line: omit. Multi-line: tip only (no "Single-line" noise).
    if snap.multiline {
        segs.push(Segment {
            text: "Multi-line".into(),
            style: Style::default().fg(Color::Gray),
            glue_next: false,
        });
    }
    join_segments(segs)
}

fn stream_segment(kind: StreamKind) -> Segment {
    let (text, color) = match kind {
        StreamKind::Idle => ("Idle", Color::DarkGray),
        StreamKind::Streaming => ("Streaming", Color::Green),
        StreamKind::Thinking => ("Thinking…", Color::Magenta),
    };
    Segment {
        text: text.to_owned(),
        style: Style::default().fg(color).add_modifier(Modifier::BOLD),
        glue_next: false,
    }
}

fn note_style(kind: NoteKind) -> Style {
    match kind {
        NoteKind::Info => Style::default().fg(Color::LightYellow),
        NoteKind::Warn => Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD),
        NoteKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn join_segments(segs: Vec<Segment>) -> Line<'static> {
    let sep = Style::default().fg(Color::DarkGray);
    let mut spans = Vec::new();
    let mut skip_dot = false;
    for seg in segs {
        if !spans.is_empty() && !skip_dot {
            spans.push(Span::styled("  ·  ", sep));
        }
        skip_dot = seg.glue_next;
        spans.push(Span::styled(seg.text, seg.style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn build_panel_lines(snap: &StatusSnapshot) -> Vec<Line<'static>> {
    let mut body = Vec::new();
    body.push(section_header("channel"));
    for line in &snap.channel_lines {
        body.push(detail_line(line.clone()));
    }
    body.push(section_header("usage"));
    for line in &snap.usage_lines {
        body.push(detail_line(line.clone()));
    }
    body.push(Line::from(Span::styled(
        snap.layout_line.clone(),
        Style::default().fg(Color::DarkGray),
    )));
    body
}

fn section_header(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

fn detail_line(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(Color::Gray)))
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_owned();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

/// Infer note kind from legacy free-text (shell migration helper).
#[must_use]
pub fn classify_note(text: &str) -> NoteKind {
    let t = text.to_ascii_lowercase();
    if t.starts_with("error") || t.starts_with("pump:") {
        NoteKind::Error
    } else if t.starts_with("warn") || t.starts_with("config") {
        NoteKind::Warn
    } else {
        NoteKind::Info
    }
}
