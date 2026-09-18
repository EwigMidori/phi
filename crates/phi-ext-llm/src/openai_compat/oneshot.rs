//! Single-response completions share the wire reader, never ProviderConversation.
use super::*;
use futures::stream;

#[async_trait]
impl OneshotModel for OpenAiCompatRuntime {
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
            .await?;
        let response = SingleResponse {
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
                if event.is_err() {
                    response.done = true;
                }
                Some((event, response))
            },
        ))))
    }
}

struct SingleResponse {
    reader: SseReader,
    complete: bool,
    done: bool,
}
impl SingleResponse {
    async fn next(&mut self) -> Result<AgentEvent, String> {
        match self.reader.next().await? {
            Some(AgentEvent::ModelResponseCompleted { response }) => {
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
            Some(event) => Ok(event),
            None if self.complete => {
                self.done = true;
                Ok(AgentEvent::Finished {
                    reason: Some("stop".into()),
                })
            }
            None => Err("provider ended without a complete response".into()),
        }
    }
}

/// Plain-text convenience adapter over the same single-response implementation.
#[async_trait]
impl OneshotText for OpenAiCompatRuntime {
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
