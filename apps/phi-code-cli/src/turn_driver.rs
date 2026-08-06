//! Turn driver port: submit user text → stream assistant deltas into Scrollback.
//!
//! Demo implementation uses an in-process echo stream. Product runtime can
//! replace this object without touching panes.

use phi_code_ui::Scrollback;

struct PendingStream {
    rest: String,
}

/// Owns the active turn stream (if any). Speaks only to [`Scrollback`].
#[derive(Default)]
pub struct TurnDriver {
    pending: Option<PendingStream>,
}

impl TurnDriver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Start a turn: push user item + begin assistant stream.
    ///
    /// Returns `false` when busy, already streaming, or `user` is empty/whitespace.
    pub fn submit(&mut self, scrollback: &mut Scrollback, user: &str) -> bool {
        if self.pending.is_some() || scrollback.is_streaming() {
            return false;
        }
        let msg = user.trim();
        if msg.is_empty() {
            return false;
        }
        scrollback.push_user(msg);
        let body = format!("### echo\n\nYou said:\n\n```\n{msg}\n```\n\n*streaming…*\n");
        scrollback.begin_assistant_stream();
        self.pending = Some(PendingStream { rest: body });
        scrollback.scroll_to_bottom();
        true
    }

    /// Advance the demo stream by one chunk.
    ///
    /// Returns `true` when the stream finished on this tick (caller clears painter).
    pub fn tick(&mut self, scrollback: &mut Scrollback) -> bool {
        let Some(p) = self.pending.as_mut() else {
            return false;
        };
        let n = p.rest.chars().take(12).map(|c| c.len_utf8()).sum::<usize>();
        if n == 0 {
            scrollback.finish_assistant_stream();
            self.pending = None;
            scrollback.scroll_to_bottom();
            return true;
        }
        let take = n.min(p.rest.len());
        let (chunk, rest) = p.rest.split_at(take);
        let chunk = chunk.to_owned();
        p.rest = rest.to_owned();
        scrollback.append_assistant_delta(&chunk);
        if p.rest.is_empty() {
            scrollback.finish_assistant_stream();
            self.pending = None;
            scrollback.scroll_to_bottom();
            return true;
        }
        scrollback.scroll_to_bottom();
        false
    }
}
