//! Single-response completions share the wire reader, never ProviderConversation.
use super::*;
use crate::ResponseStep;
use futures::stream;

#[async_trait]
impl OneshotModel for LlmRuntime {
    async fn generate(&self, request: OneshotRequest) -> Result<AgentRun, String> {
        if !request.input.has_input() {
            return Err("oneshot input empty".into());
        }
        let prefix = AgentPrefix {
            preamble: request.instructions,
            ..AgentPrefix::default()
        };
        let history = [TurnItem::User {
            content: request.input,
        }];
        let reader = self
            .open_response(&request.session_id, &prefix, &history, &request.cancel)
            .await
            .map_err(ProviderError::into_message)?;
        let failure = Arc::new(phi_kernel::RunFailure::default());
        let response = SingleResponse {
            failure: failure.clone(),
            reader,
            complete: false,
            done: false,
        };
        // No tool execution scope or producer exists here. Dropping the owned
        // stream closes the HTTP reader, including cancellation/error paths.
        Ok(AgentRun::new(Box::pin(stream::unfold(
            response,
            |mut response| async move {
                if response.done {
                    return None;
                }
                let event = response.next().await;
                if let Err(error) = &event {
                    response.failure.record(error.failure_disposition());
                    response.done = true;
                    response.reader.close();
                }
                Some((event.map_err(ProviderError::into_message), response))
            },
        )))
        .with_failure_reporter(failure))
    }
}

struct SingleResponse {
    failure: Arc<phi_kernel::RunFailure>,
    reader: Box<dyn ProviderResponse>,
    complete: bool,
    done: bool,
}
impl SingleResponse {
    async fn next(&mut self) -> Result<AgentEvent, ProviderError> {
        match self.reader.next().await? {
            ResponseStep::ReadyToSeal => {
                let response = self.reader.seal(&NoTools)?;
                if self.complete || !response.complete {
                    return Err("oneshot response incomplete or repeated".into());
                }
                if response.rows.iter().any(|row| {
                    !matches!(
                        row.item,
                        TurnItem::Assistant { .. } | TurnItem::Reasoning { .. }
                    )
                }) {
                    return Err("provider requested tools for a turn with no tools enabled".into());
                }
                self.complete = true;
                Ok(AgentEvent::ModelResponseCompleted { response })
            }
            ResponseStep::Observation(event) => {
                if !matches!(
                    event,
                    AgentEvent::ResponseStarted { .. }
                        | AgentEvent::TextDelta { .. }
                        | AgentEvent::ReasoningDelta { .. }
                        | AgentEvent::Usage { .. }
                ) {
                    return Err(
                        "protocol emitted an effect instead of a response observation".into(),
                    );
                }
                Ok(event)
            }
            ResponseStep::Ended if self.complete => {
                self.reader.close();
                self.done = true;
                Ok(AgentEvent::Finished {
                    reason: Some("stop".into()),
                })
            }
            ResponseStep::Ended => Err("provider ended without a complete response".into()),
        }
    }
}

/// Plain-text convenience adapter over the same single-response implementation.
#[async_trait]
impl OneshotText for LlmRuntime {
    async fn complete(&self, input: &str) -> Result<String, String> {
        let mut run = self
            .generate(OneshotRequest {
                session_id: SessionId::generate(),
                input: MessageContent::text(input),
                instructions: Vec::new(),
                cancel: TurnCancel::new(),
            })
            .await?;
        let result = async {
            let mut text = String::new();
            while let Some(event) = run.next().await {
                match event? {
                    AgentEvent::ModelResponseCompleted { response } => {
                        for row in response.rows {
                            if let TurnItem::Assistant { content } = row.item {
                                text.push_str(&content);
                            }
                        }
                    }
                    AgentEvent::Finished { .. } => return Ok(text),
                    AgentEvent::Error { message } => return Err(message),
                    _ => {}
                }
            }
            Err("oneshot response ended early".into())
        }
        .await;
        run.close_and_join().await?;
        result
    }
}

impl Drop for SingleResponse {
    fn drop(&mut self) {
        self.reader.close();
    }
}
