use crate::{KernelError, ModelResponseId, Result, ToolCallId, ToolName};
pub(crate) struct ToolLedger {
    open: Vec<(ModelResponseId, ToolCallId, ToolName)>,
}
impl ToolLedger {
    pub fn new() -> Self {
        Self { open: Vec::new() }
    }
    pub fn open(
        &mut self,
        response: ModelResponseId,
        id: ToolCallId,
        name: ToolName,
    ) -> Result<()> {
        if self.open.iter().any(|(r, i, _)| r == &response && i == &id) {
            return Err(KernelError::InvalidArgument(
                "duplicate open tool call".into(),
            ));
        }
        self.open.push((response, id, name));
        Ok(())
    }
    pub fn close(&mut self, response: &ModelResponseId, id: &ToolCallId) -> Result<ToolName> {
        let index = self
            .open
            .iter()
            .position(|(r, i, _)| r == response && i == id)
            .ok_or_else(|| KernelError::InvalidArgument("tool result has no open call".into()))?;
        Ok(self.open.remove(index).2)
    }
    pub fn seal_incomplete(&mut self) -> Vec<(ModelResponseId, ToolCallId, ToolName)> {
        std::mem::take(&mut self.open)
    }
}
