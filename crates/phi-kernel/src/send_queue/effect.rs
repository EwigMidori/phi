//! Sole durable commit path; observations are projected only after success.
use super::GenerationJob;
use crate::{
    EventBus, GenerationCommit, GenerationStatus, JobId, KernelEvent, MessageId, Result, SessionId,
    Transcript, TurnItem,
};

pub(crate) enum TurnOutcome {
    Completed {
        job: GenerationJob,
        assistant_message_id: MessageId,
        content: String,
    },
    Aborted {
        job: GenerationJob,
        assistant_message_id: MessageId,
        partial: String,
    },
    Failed {
        job: GenerationJob,
        message: String,
    },
}
impl TurnOutcome {
    pub fn job_id(&self) -> &JobId {
        match self {
            Self::Completed { job, .. } | Self::Aborted { job, .. } | Self::Failed { job, .. } => {
                &job.job_id
            }
        }
    }
}
pub(crate) enum CommitUnit {
    Generation(GenerationCommit),
    Notice(KernelEvent),
    Terminal(TurnOutcome),
}
pub(crate) struct EffectApplier<'a> {
    session_id: &'a SessionId,
    transcript: &'a dyn Transcript,
    bus: &'a EventBus,
}
impl<'a> EffectApplier<'a> {
    pub fn fault(&self, job_id: &JobId, message: String) -> Result<()> {
        let version = self.transcript.version()?;
        self.commit(CommitUnit::Notice(KernelEvent::GenerationError {
            session_id: self.session_id.clone(),
            job_id: job_id.clone(),
            message,
            version,
        }))
    }
    pub fn start(&self, job: &GenerationJob, assistant_message_id: &MessageId) -> Result<()> {
        let version = self
            .transcript
            .commit_generation(&GenerationCommit::Start { job: job.clone() })?;
        let _ = self.bus.send(KernelEvent::GenerationStart {
            session_id: self.session_id.clone(),
            job_id: job.job_id.clone(),
            user_message_id: job.user_message_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
            version,
        });
        Ok(())
    }
    pub fn new(
        session_id: &'a SessionId,
        transcript: &'a dyn Transcript,
        bus: &'a EventBus,
    ) -> Self {
        Self {
            session_id,
            transcript,
            bus,
        }
    }
    pub fn commit(&self, unit: CommitUnit) -> Result<()> {
        match unit {
            CommitUnit::Notice(event) => {
                let _ = self.bus.send(event);
            }
            CommitUnit::Generation(commit) => {
                let version = self.transcript.commit_generation(&commit)?;
                let job = commit.job();
                match &commit {
                    GenerationCommit::Response { response, .. } => {
                        let assistant_message_ids = response
                            .rows
                            .iter()
                            .filter(|row| matches!(row.item, TurnItem::Assistant { .. }))
                            .map(|row| row.id.clone())
                            .collect();
                        let _ = self
                            .bus
                            .send(KernelEvent::GenerationModelResponseCommitted {
                                session_id: self.session_id.clone(),
                                job_id: job.job_id.clone(),
                                response_id: response.id.clone(),
                                assistant_message_ids,
                                version,
                            });
                        for row in &response.rows {
                            if let TurnItem::ToolCall {
                                tool_call_id,
                                tool_name,
                                input,
                            } = &row.item
                            {
                                let _ = self.bus.send(KernelEvent::GenerationToolCall {
                                    session_id: self.session_id.clone(),
                                    job_id: job.job_id.clone(),
                                    tool_call_id: tool_call_id.clone(),
                                    tool_name: tool_name.clone(),
                                    input: input.observation(),
                                });
                            }
                        }
                    }
                    GenerationCommit::ToolResult {
                        tool_call_id,
                        output,
                        status,
                        ..
                    } => {
                        let _ = self.bus.send(KernelEvent::GenerationToolResult {
                            session_id: self.session_id.clone(),
                            job_id: job.job_id.clone(),
                            tool_call_id: tool_call_id.clone(),
                            output: output.clone(),
                            status: *status,
                        });
                    }
                    _ => {}
                }
            }
            CommitUnit::Terminal(outcome) => self.apply_terminal(outcome)?,
        }
        Ok(())
    }
    fn apply_terminal(&self, outcome: TurnOutcome) -> Result<()> {
        let (job, status) = match &outcome {
            TurnOutcome::Completed { job, .. } => (job, GenerationStatus::Completed),
            TurnOutcome::Aborted { job, .. } => (job, GenerationStatus::Stopped),
            TurnOutcome::Failed { job, message } => (
                job,
                GenerationStatus::Failed {
                    message: message.clone(),
                },
            ),
        };
        let version = self
            .transcript
            .commit_generation(&GenerationCommit::Finish {
                job: job.clone(),
                status,
            })?;
        let event = match outcome {
            TurnOutcome::Completed {
                job,
                assistant_message_id,
                content,
            } => KernelEvent::GenerationDone {
                session_id: self.session_id.clone(),
                job_id: job.job_id,
                assistant_message_id,
                content,
                version,
            },
            TurnOutcome::Aborted {
                job,
                assistant_message_id,
                partial,
            } => KernelEvent::GenerationStopped {
                session_id: self.session_id.clone(),
                job_id: job.job_id,
                assistant_message_id,
                partial,
                version,
            },
            TurnOutcome::Failed { job, message } => KernelEvent::GenerationError {
                session_id: self.session_id.clone(),
                job_id: job.job_id,
                message,
                version,
            },
        };
        let _ = self.bus.send(event);
        Ok(())
    }
}
