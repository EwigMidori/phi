//! Format application channel/usage snapshots into status chrome strings.
//!
//! **Pure free functions only** (inputs → strings; no object or process I/O).
//! Token *aggregation* stays in [`crate::turn_driver`].

use crate::turn_driver::{ChannelInfo, UsageInfo};

/// Compact bar fragment: `used / window` (e.g. `12K / 128K`). Always shows window.
#[must_use]
pub fn usage_bar(usage: &UsageInfo) -> String {
    let window = format_token_qty(usage.context_window);
    match usage.context_used() {
        Some(used) => format!("{} / {}", format_token_qty(used), window),
        None => format!("— / {window}"),
    }
}

/// Expanded panel: channel identity lines.
#[must_use]
pub fn channel_detail_lines(ch: &ChannelInfo) -> Vec<String> {
    vec![
        format!("  model:  {}", ch.model_id),
        format!("  style:  {}", ch.api_style),
        format!("  base:   {}", ch.api_base),
        format!(
            "  window: {} ({} tokens)",
            format_token_qty(ch.context_window),
            ch.context_window
        ),
    ]
}

/// Expanded panel: usage detail lines.
#[must_use]
pub fn usage_detail_lines(usage: &UsageInfo) -> Vec<String> {
    let mut lines = Vec::new();
    match usage.context_used() {
        Some(used) => lines.push(format!(
            "  context: {} / {} ({} / {} tokens)",
            format_token_qty(used),
            format_token_qty(usage.context_window),
            used,
            usage.context_window
        )),
        None => lines.push(format!(
            "  context: — / {} (window {}; waiting for provider usage)",
            format_token_qty(usage.context_window),
            usage.context_window
        )),
    }
    if let Some(u) = &usage.last {
        let mut last = Vec::new();
        if let Some(p) = u.prompt_tokens {
            last.push(format!("prompt {p}"));
        }
        if let Some(c) = u.completion_tokens {
            last.push(format!("completion {c}"));
        }
        if let Some(t) = u.total_tokens {
            last.push(format!("total {t}"));
        }
        if !last.is_empty() {
            lines.push(format!("  last turn: {}", last.join(" · ")));
        }
    }
    let mut sigma = Vec::new();
    if usage.session_prompt > 0 {
        sigma.push(format!("prompt {}", usage.session_prompt));
    }
    if usage.session_completion > 0 {
        sigma.push(format!("completion {}", usage.session_completion));
    }
    if usage.session_total > 0 {
        sigma.push(format!("total {}", usage.session_total));
    }
    if !sigma.is_empty() {
        lines.push(format!("  session Σ: {}", sigma.join(" · ")));
    }
    lines
}

/// Human quantity for the bar: `999`, `12K`, `1M`, `1.5M` (provider counts only).
fn format_token_qty(n: u64) -> String {
    if n >= 1_000_000 {
        let whole = n / 1_000_000;
        let tenths = (n % 1_000_000) / 100_000;
        if tenths == 0 {
            format!("{whole}M")
        } else {
            format!("{whole}.{tenths}M")
        }
    } else if n >= 1000 {
        let k = n.div_ceil(1000);
        format!("{k}K")
    } else {
        n.to_string()
    }
}
