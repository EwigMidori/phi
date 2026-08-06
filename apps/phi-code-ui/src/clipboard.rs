//! Host system clipboard object (arboard), for TextArea + selection copy.

use arboard::Clipboard;
use xai_ratatui_textarea::ClipboardProvider;

/// System clipboard: answers get/set messages via [`ClipboardProvider`].
#[derive(Debug, Default)]
pub struct SystemClipboard;

impl SystemClipboard {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Copy text; ignore host errors silently (demo path).
    pub fn copy_text(&mut self, text: &str) {
        ClipboardProvider::set(self, text);
    }

    /// Read text if the host provides any.
    #[must_use]
    pub fn paste_text(&mut self) -> Option<String> {
        ClipboardProvider::get(self)
    }
}

impl ClipboardProvider for SystemClipboard {
    fn get(&mut self) -> Option<String> {
        Clipboard::new().ok()?.get_text().ok()
    }

    fn set(&mut self, text: &str) {
        if let Ok(mut clip) = Clipboard::new() {
            let _ = clip.set_text(text);
        }
    }
}
