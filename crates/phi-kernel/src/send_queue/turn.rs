//! A generation owns its active response projection and commits every completed
//! response before the adapter may execute tools on its next poll.
use super::{
    GenerationJob,
    effect::{CommitUnit, EffectApplier, TurnOutcome},
    epoch::Epoch,
    tool_ledger::ToolLedger,
};
use crate::{
    AgentEvent, AgentRun, GenerationCommit, JobId, KernelEvent, MessageId, ModelResponse,
    ModelResponseId, Result, SessionId, ToolCallSealPolicy, ToolResultStatus, TranscriptRow,
    TurnCancel, TurnItem,
};

pub(crate) struct GenerationTurn {
    job: GenerationJob,
    claimed_epoch: Epoch,
    history: Vec<TurnItem>,
    assistant_message_id: MessageId,
    response_id: ModelResponseId,
    buffer: String,
    reasoning: String,
    final_text: String,
    tool_ledger: ToolLedger,
}
impl GenerationTurn {
    pub fn begin(job: GenerationJob, claimed_epoch: Epoch, history: Vec<TurnItem>) -> Self {
        Self {
            job,
            claimed_epoch,
            history,
            assistant_message_id: MessageId::generate(),
            response_id: ModelResponseId::generate(),
            buffer: String::new(),
            reasoning: String::new(),
            final_text: String::new(),
            tool_ledger: ToolLedger::new(),
        }
    }
    pub fn session_id(&self) -> &SessionId {
        &self.job.session_id
    }
    pub fn job_id(&self) -> &JobId {
        &self.job.job_id
    }
    pub fn claimed_epoch(&self) -> Epoch {
        self.claimed_epoch
    }
    pub fn history(&self) -> &[TurnItem] {
        &self.history
    }
    pub fn start(&self, applier: &EffectApplier<'_>) -> Result<()> {
        applier.start(&self.job, &self.assistant_message_id)
    }
    fn partial(&mut self, applier: &EffectApplier<'_>) -> Result<()> {
        let mut rows = Vec::new();
        if !self.reasoning.is_empty() {
            rows.push(TranscriptRow::new(
                MessageId::generate(),
                TurnItem::Reasoning {
                    content: std::mem::take(&mut self.reasoning),
                },
            ));
        }
        if !self.buffer.is_empty() {
            rows.push(TranscriptRow::new(
                self.assistant_message_id.clone(),
                TurnItem::Assistant {
                    content: std::mem::take(&mut self.buffer),
                },
            ));
        }
        if !rows.is_empty() {
            applier.commit(CommitUnit::Generation(GenerationCommit::Response {
                job: self.job.clone(),
                response: ModelResponse {
                    id: self.response_id.clone(),
                    rows,
                    continuation: None,
                    complete: false,
                },
            }))?;
        }
        Ok(())
    }
    pub async fn drive(
        mut self,
        mut run: AgentRun,
        mut is_cancelled: impl FnMut() -> bool,
        cancel: &TurnCancel,
        seal: ToolCallSealPolicy,
        applier: &EffectApplier<'_>,
    ) -> Result<TurnOutcome> {
        let mut error = None;
        let mut stopped = false;
        let mut finished = false;
        let mut commit_error = None;
        loop {
            let item = tokio::select! {biased;()=cancel.cancelled()=>{stopped=true;break;} item=run.next()=>item};
            if is_cancelled() || cancel.is_cancelled() {
                cancel.cancel();
                stopped = true;
                break;
            }
            let Some(item) = item else {
                error = Some("agent stream ended without a terminal event".into());
                break;
            };
            let event = match item {
                Ok(event) => event,
                Err(message) => {
                    error = Some(message);
                    break;
                }
            };
            let result: Result<()> = match event {
                AgentEvent::ResponseStarted {
                    response_id,
                    assistant_message_id,
                } => {
                    self.response_id = response_id;
                    self.assistant_message_id = assistant_message_id;
                    Ok(())
                }
                AgentEvent::TextDelta { text } => {
                    self.buffer.push_str(&text);
                    applier.commit(CommitUnit::Notice(KernelEvent::GenerationTextDelta {
                        session_id: self.session_id().clone(),
                        job_id: self.job_id().clone(),
                        assistant_message_id: self.assistant_message_id.clone(),
                        text,
                    }))
                }
                AgentEvent::ReasoningDelta { text } => {
                    self.reasoning.push_str(&text);
                    applier.commit(CommitUnit::Notice(KernelEvent::GenerationReasoningDelta {
                        session_id: self.session_id().clone(),
                        job_id: self.job_id().clone(),
                        assistant_message_id: self.assistant_message_id.clone(),
                        text,
                    }))
                }
                AgentEvent::ModelResponseCompleted { response } => {
                    let mut result =
                        applier.commit(CommitUnit::Generation(GenerationCommit::Response {
                            job: self.job.clone(),
                            response: response.clone(),
                        }));
                    if result.is_ok() {
                        self.final_text.clear();
                        for row in &response.rows {
                            match &row.item {
                                TurnItem::ToolCall {
                                    tool_call_id,
                                    tool_name,
                                    ..
                                } => {
                                    if let Err(error) = self.tool_ledger.open(
                                        response.id.clone(),
                                        tool_call_id.clone(),
                                        tool_name.clone(),
                                    ) {
                                        result = Err(error);
                                        break;
                                    }
                                }
                                TurnItem::Assistant { content } => {
                                    self.final_text.push_str(content);
                                    self.assistant_message_id = row.id.clone();
                                }
                                _ => {}
                            }
                        }
                        self.buffer.clear();
                        self.reasoning.clear();
                    }
                    result
                }
                AgentEvent::ToolResult {
                    response_id,
                    tool_call_id,
                    output,
                    status,
                } => match self.tool_ledger.close(&response_id, &tool_call_id) {
                    Ok(tool_name) => {
                        applier.commit(CommitUnit::Generation(GenerationCommit::ToolResult {
                            job: self.job.clone(),
                            response_id,
                            tool_call_id,
                            tool_name,
                            output,
                            status,
                        }))
                    }
                    Err(error) => Err(error),
                },
                AgentEvent::Usage { usage } => {
                    applier.commit(CommitUnit::Notice(KernelEvent::GenerationUsage {
                        session_id: self.session_id().clone(),
                        job_id: self.job_id().clone(),
                        usage,
                    }))
                }
                AgentEvent::Error { message } => {
                    error = Some(message);
                    break;
                }
                AgentEvent::Finished { .. } => {
                    finished = true;
                    break;
                }
                AgentEvent::ToolApprovalRequired { .. } => {
                    error = Some("runtime requested unsupported approval".into());
                    break;
                }
                AgentEvent::Unknown { kind, payload } => {
                    applier.commit(CommitUnit::Notice(KernelEvent::GenerationAgentUnknown {
                        session_id: self.session_id().clone(),
                        job_id: self.job_id().clone(),
                        kind,
                        payload,
                    }))
                }
            };
            if let Err(error) = result {
                commit_error = Some(error);
                break;
            }
        }
        // Closing owns cancellation and joins subprocesses even after commit failure.
        let close = run.close_and_join().await;
        if let Some(error) = commit_error {
            return Err(error);
        }
        if let Err(message) = close {
            error = Some(message);
        }
        let aborting = stopped || is_cancelled();
        let partial = self.buffer.clone();
        self.partial(applier)?;
        if seal.should_seal(aborting) {
            for (response_id, tool_call_id, tool_name) in self.tool_ledger.seal_incomplete() {
                applier.commit(CommitUnit::Generation(GenerationCommit::ToolResult {
                    job: self.job.clone(),
                    response_id,
                    tool_call_id,
                    tool_name,
                    output: serde_json::json!({}),
                    status: ToolResultStatus::Incomplete,
                }))?;
            }
        }
        Ok(if aborting {
            TurnOutcome::Aborted {
                job: self.job,
                assistant_message_id: self.assistant_message_id,
                partial,
            }
        } else if let Some(message) = error {
            self.into_failed(message)
        } else if finished {
            self.into_completed()
        } else {
            self.into_failed("generation ended unexpectedly")
        })
    }
    pub fn into_aborted(self) -> TurnOutcome {
        TurnOutcome::Aborted {
            job: self.job,
            assistant_message_id: self.assistant_message_id,
            partial: self.buffer,
        }
    }
    pub fn into_failed(self, message: impl Into<String>) -> TurnOutcome {
        TurnOutcome::Failed {
            job: self.job,
            message: message.into(),
        }
    }
    fn into_completed(self) -> TurnOutcome {
        TurnOutcome::Completed {
            job: self.job,
            assistant_message_id: self.assistant_message_id,
            content: self.final_text,
        }
    }
}
