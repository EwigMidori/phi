//! SSE byte framing only. Provider completion markers belong to the protocol.
use crate::ProviderError;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use phi_kernel::TurnCancel;
use std::pin::Pin;

#[derive(Debug)]
pub(crate) struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

pub(crate) struct SseFramer {
    stream: Option<Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>>,
    buffer: Vec<u8>,
    data: Vec<String>,
    event: Option<String>,
    cancel: TurnCancel,
    remaining: Option<usize>,
    frame_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderErrorKind;
    use futures::stream;

    #[tokio::test]
    async fn byte_chunks_preserve_unicode_events_and_require_the_final_blank_line() {
        let fixture = ": comment\r\nevent: answer\r\ndata: 中\r\ndata: 文\r\n\r\ndata: done\n\n";
        let chunks: Vec<_> = fixture
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, reqwest::Error>(Bytes::copy_from_slice(&[*byte])))
            .collect();
        let mut reader = SseFramer::new(stream::iter(chunks), TurnCancel::new());
        let first = reader.next().await.unwrap().unwrap();
        assert_eq!(first.event.as_deref(), Some("answer"));
        assert_eq!(first.data, "中\n文");
        assert_eq!(reader.next().await.unwrap().unwrap().data, "done");
        assert!(reader.next().await.unwrap().is_none());
        for truncated in ["data: complete-json\n", "data: partial"] {
            let mut reader = SseFramer::new(
                stream::iter([Ok::<_, reqwest::Error>(Bytes::from_static(
                    truncated.as_bytes(),
                ))]),
                TurnCancel::new(),
            );
            assert_eq!(
                reader.next().await.unwrap_err().kind(),
                ProviderErrorKind::Protocol
            );
        }
    }

    #[tokio::test]
    async fn drain_budget_and_cancel_close_only_the_current_reader() {
        let data = format!("data: {}\n\n", "x".repeat(256 * 1024));
        let mut reader = SseFramer::new(
            stream::iter([Ok::<_, reqwest::Error>(Bytes::from(data))]),
            TurnCancel::new(),
        );
        reader.begin_usage_drain();
        assert!(reader.next().await.unwrap().is_none());
        let cancel = TurnCancel::new();
        let mut reader = SseFramer::new(
            stream::pending::<Result<Bytes, reqwest::Error>>(),
            cancel.clone(),
        );
        cancel.cancel();
        assert_eq!(
            reader.next().await.unwrap_err().kind(),
            ProviderErrorKind::Cancelled
        );
    }

    #[tokio::test]
    async fn frame_limit_counts_consumed_data_lines_not_only_the_unread_buffer() {
        let chunks = (0..9)
            .map(|_| {
                Ok::<_, reqwest::Error>(Bytes::from(format!("data: {}\n", "x".repeat(1024 * 1024))))
            })
            .collect::<Vec<_>>();
        let mut reader = SseFramer::new(stream::iter(chunks), TurnCancel::new());
        assert_eq!(
            reader.next().await.unwrap_err().kind(),
            ProviderErrorKind::Protocol
        );
    }
}
impl SseFramer {
    pub(crate) fn new<S>(stream: S, cancel: TurnCancel) -> Self
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        Self {
            stream: Some(Box::pin(stream)),
            buffer: Vec::new(),
            data: Vec::new(),
            event: None,
            cancel,
            remaining: None,
            frame_bytes: 0,
        }
    }
    pub(crate) fn begin_usage_drain(&mut self) {
        self.remaining
            .get_or_insert_with(|| (256 * 1024usize).saturating_sub(self.frame_bytes));
    }
    pub(crate) fn close(&mut self) {
        self.stream = None;
        self.buffer.clear();
        self.data.clear();
        self.event = None;
    }
    pub(crate) async fn next(&mut self) -> Result<Option<SseFrame>, ProviderError> {
        loop {
            if self.cancel.is_cancelled() {
                self.close();
                return Err(ProviderError::cancelled());
            }
            while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if let Some(remaining) = &mut self.remaining {
                    if index + 1 > *remaining {
                        self.close();
                        return Ok(None);
                    }
                    *remaining -= index + 1;
                }
                self.frame_bytes += index + 1;
                if self.frame_bytes > 8 * 1024 * 1024 {
                    self.close();
                    return Err("provider SSE frame exceeds limit".into());
                }
                let line: Vec<_> = self.buffer.drain(..=index).collect();
                let line = std::str::from_utf8(&line)
                    .map_err(|_| "invalid UTF-8 in provider stream")?
                    .trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    self.frame_bytes = 0;
                    let event = self.event.take();
                    if !self.data.is_empty() {
                        return Ok(Some(SseFrame {
                            event,
                            data: std::mem::take(&mut self.data).join("\n"),
                        }));
                    }
                } else if let Some(value) = line.strip_prefix("data:") {
                    self.data
                        .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
                } else if let Some(value) = line.strip_prefix("event:") {
                    self.event = Some(value.trim().to_owned());
                }
            }
            if self
                .remaining
                .is_some_and(|remaining| self.buffer.len() >= remaining)
            {
                self.close();
                return Ok(None);
            }
            let Some(stream) = &mut self.stream else {
                return Ok(None);
            };
            let next = tokio::select! { () = self.cancel.cancelled() => return Err(ProviderError::cancelled()), value = stream.next() => value };
            match next {
                Some(Ok(bytes)) => {
                    let allowed = self.remaining.map_or(bytes.len(), |remaining| {
                        remaining.saturating_sub(self.buffer.len()).min(bytes.len())
                    });
                    self.buffer.extend_from_slice(&bytes[..allowed]);
                    if self.buffer.len() > 8 * 1024 * 1024 {
                        self.close();
                        return Err("provider SSE frame exceeds limit".into());
                    }
                }
                Some(Err(_)) => {
                    self.close();
                    return Err(ProviderError::transport(
                        "provider response transport interrupted",
                    ));
                }
                None => {
                    self.stream = None;
                    if !self.buffer.is_empty() {
                        return Err("provider stream ended inside an SSE frame".into());
                    }
                    if !self.data.is_empty() {
                        return Err(ProviderError::protocol(
                            "provider stream ended before the SSE event boundary",
                        ));
                    }
                    return Ok(None);
                }
            }
        }
    }
}
