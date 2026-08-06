//! # phi-code-ui
//!
//! Terminal UI objects for phi-code (Alan Kay style: autonomous objects,
//! private state, collaboration by message).
//!
//! Kernel [`TurnItem`] remains conversation SoT; this crate never invents a
//! parallel history type.

#![forbid(unsafe_code)]

mod clipboard;
mod layout;
mod markdown;
mod painter;
mod paste;
mod scrollback;
mod scrollbar;
mod selection;

// Crate root is the public surface; modules stay private.
pub use clipboard::SystemClipboard;
pub use layout::HorizontalLayout;
pub use markdown::ProductMarkdown;
pub use painter::ScrollbackPainter;
pub use paste::PastePolicy;
pub use scrollback::{Accent, Scrollback, ThinkingLayout, VisibleSegment};
pub use scrollbar::{HistoryScrollbar, ScrollInfo, ScrollbarClick};
pub use selection::{AutoScrollDirection, DragAutoScrollState, Selection, TextHit, TextSelection};

pub const UI_NAME: &str = "phi-code-ui";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
