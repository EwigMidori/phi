//! # phi-code-ui
//!
//! Terminal UI objects for phi-code (Alan Kay style: autonomous objects,
//! private state, collaboration by message).
//!
//! Kernel [`TurnItem`] remains conversation SoT; this crate never invents a
//! parallel history type.

#![forbid(unsafe_code)]

pub mod clipboard;
pub mod layout;
pub mod markdown;
pub mod painter;
pub mod paste;
pub mod scrollback;
pub mod scrollbar;
pub mod selection;

pub use clipboard::SystemClipboard;
pub use layout::HorizontalLayout;
pub use markdown::ProductMarkdown;
pub use painter::ScrollbackPainter;
pub use paste::PastePolicy;
pub use phi_kernel::{ToolCallId, ToolName, ToolResultStatus, TurnItem};
pub use scrollback::{Accent, Scrollback, ThinkingLayout, VisibleSegment};
pub use scrollbar::{HistoryScrollbar, ScrollInfo, ScrollbarClick};
pub use selection::{AutoScrollDirection, DragAutoScrollState, Selection, TextHit, TextSelection};

pub const UI_NAME: &str = "phi-code-ui";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
