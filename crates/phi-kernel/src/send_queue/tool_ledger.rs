//! Open tool-call ledger for one generation turn.
//!
//! Tracks [`ToolCallId`] → [`ToolName`] while a call is open; close is idempotent;
//! seal drains remaining open entries as incomplete candidates.

use std::collections::HashMap;

use crate::agent::{ToolCallId, ToolName};

/// First-class open/close/seal for in-flight tool calls (replaces raw HashMap dual paths).
#[derive(Debug, Default)]
pub(crate) struct ToolLedger {
    /// Open tool calls awaiting a result.
    open: HashMap<ToolCallId, ToolName>,
}

impl ToolLedger {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            open: HashMap::new(),
        }
    }

    pub(crate) fn open(&mut self, tool_call_id: ToolCallId, tool_name: ToolName) {
        self.open.insert(tool_call_id, tool_name);
    }

    /// Close a known open call. Returns the tool name if it was open; `None` if unknown (idempotent).
    pub(crate) fn close(&mut self, tool_call_id: &ToolCallId) -> Option<ToolName> {
        self.open.remove(tool_call_id)
    }

    /// Drain all still-open calls as incomplete seal candidates.
    pub(crate) fn seal_incomplete(&mut self) -> Vec<(ToolCallId, ToolName)> {
        self.open.drain().collect()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}
