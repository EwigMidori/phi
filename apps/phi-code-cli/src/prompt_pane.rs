//! Prompt pane: TextArea + paste policy + multiline preference.

use crossterm::event::{KeyEvent, MouseEvent};
use phi_code_ui::PastePolicy;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, StatefulWidgetRef};
use ratatui::Frame;
use xai_ratatui_textarea::{TextArea, TextAreaState};

/// Composer object. Shell routes focus/keys/paste here.
pub struct PromptPane {
    textarea: TextArea,
    ta_state: TextAreaState,
    ta_area: Rect,
    paste_policy: PastePolicy,
    multiline_prefer: bool,
    last_inner_w: u16,
    hit: Rect,
}

impl PromptPane {
    #[must_use]
    pub fn new(clipboard: Box<dyn xai_ratatui_textarea::ClipboardProvider>) -> Self {
        let mut textarea = TextArea::new();
        textarea.set_clipboard_provider(clipboard);
        Self {
            textarea,
            ta_state: TextAreaState::default(),
            ta_area: Rect::default(),
            paste_policy: PastePolicy::new(),
            multiline_prefer: false,
            last_inner_w: 40,
            hit: Rect::default(),
        }
    }

    #[must_use]
    pub fn hit(&self) -> Rect {
        self.hit
    }

    #[must_use]
    pub fn is_multiline(&self) -> bool {
        self.multiline_prefer
    }

    pub fn toggle_multiline(&mut self) {
        self.multiline_prefer = !self.multiline_prefer;
    }

    /// Vertical budget for the prompt chrome (content + top border row).
    #[must_use]
    pub fn desired_height(&self) -> u16 {
        let content_h = self
            .textarea
            .desired_height(self.last_inner_w.max(1))
            .clamp(1, if self.multiline_prefer { 10 } else { 6 });
        content_h.saturating_add(2).clamp(3, 12)
    }

    pub fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, focused: bool) {
        self.hit = area;
        let title = if focused { " prompt * " } else { " prompt " };
        let block = Block::default()
            .borders(Borders::TOP)
            .title(title)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.last_inner_w = inner.width;
        self.ta_area = inner;

        if inner.width == 0 || inner.height == 0 {
            return;
        }
        StatefulWidgetRef::render_ref(
            &&self.textarea,
            inner,
            frame.buffer_mut(),
            &mut self.ta_state,
        );
        if focused
            && let Some((cx, cy)) = self
                .textarea
                .cursor_pos_with_state(inner, self.ta_state)
        {
            frame.set_cursor_position(ratatui::layout::Position { x: cx, y: cy });
        }
    }

    pub fn apply_paste(&mut self, text: &str) {
        self.paste_policy.apply(&mut self.textarea, text);
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let _ = self
            .textarea
            .handle_mouse(mouse, self.ta_area, self.ta_state);
    }

    pub fn input_key(&mut self, key: KeyEvent) {
        self.textarea.input(key);
    }

    pub fn insert_newline(&mut self) {
        self.textarea.insert_str("\n");
    }

    /// Trimmed composer text for submit (does not clear).
    #[must_use]
    pub fn submit_candidate(&self) -> String {
        self.textarea.text().trim().to_owned()
    }

    pub fn clear(&mut self) {
        self.textarea.set_text("");
    }
}
