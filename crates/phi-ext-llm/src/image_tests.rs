//! DeepSeek Files / Chat Completions / Responses protocol fixtures, 2026-09-11.
use crate::{images::PreparedImage, *};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures::StreamExt;
use phi_kernel::{
    AgentPrefix, AgentRuntime, ContentPart, ImageId, JobId, MessageContent, SessionId, TailState,
    ToolCallSealPolicy, TurnCancel, TurnItem, TurnRequest,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Default)]
struct MemoryCache(Mutex<HashMap<FileCacheKey, FileReference>>);
impl FileReferenceCache for MemoryCache {
    fn lookup(&self, key: &FileCacheKey) -> Result<Option<FileReference>, String> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn remember(&self, key: &FileCacheKey, reference: &FileReference) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(key.clone(), reference.clone());
        Ok(())
    }
    fn forget(&self, key: &FileCacheKey, id: &ProviderFileId) -> Result<(), String> {
        let mut cache = self.0.lock().unwrap();
        if cache.get(key).is_some_and(|value| &value.id == id) {
            cache.remove(key);
        }
        Ok(())
    }
}
struct Source {
    bytes: Bytes,
    reads: AtomicUsize,
}
impl Source {
    fn new() -> Self {
        Self {
            bytes: Bytes::from_static(b"immutable image bytes"),
            reads: AtomicUsize::new(0),
        }
    }
}
#[async_trait]
impl ImageSource for Source {
    async fn metadata(&self, _: &SessionId, _: &ImageId) -> Result<ImageMetadata, String> {
        Ok(ImageMetadata {
            digest: ImageDigest::of(&self.bytes),
            mime_type: "image/png".into(),
            byte_len: self.bytes.len() as u64,
            width: 64,
            height: 64,
        })
    }
    async fn read(&self, _: &SessionId, _: &ImageId) -> Result<Bytes, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.bytes.clone())
    }
}
struct ServerState {
    uploads: AtomicUsize,
    calls: Mutex<Vec<Value>>,
    block: AtomicBool,
    gates: Semaphore,
    reject: AtomicU16,
    reject_forever: AtomicBool,
    probes: AtomicUsize,
}
struct Server {
    state: Arc<ServerState>,
    base: ApiBase,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start() -> Self {
        let state = Arc::new(ServerState {
            uploads: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            block: AtomicBool::new(false),
            gates: Semaphore::new(0),
            reject: AtomicU16::new(0),
            reject_forever: AtomicBool::new(false),
            probes: AtomicUsize::new(0),
        });
        let router = Router::new()
            .route("/files", post(upload))
            .route("/files/{id}", get(info))
            .route("/chat/completions", post(complete))
            .route("/responses", post(complete))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = ApiBase::try_new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, base, task }
    }
    fn config(&self, style: ApiStyle, key: &str) -> LlmConfig {
        LlmConfig::new(
            self.base.clone(),
            ApiKey::try_new(key).unwrap(),
            ModelId::try_new("deepseek-flash").unwrap(),
            style,
        )
    }
}
async fn upload(State(state): State<Arc<ServerState>>, mut parts: Multipart) -> Response {
    let mut purpose = None;
    let mut file = None;
    let mut expiry = None;
    while let Some(part) = parts.next_field().await.unwrap() {
        match part.name().unwrap_or("") {
            "purpose" => purpose = Some(part.text().await.unwrap()),
            "file" => file = Some(part.bytes().await.unwrap()),
            "expires_after[seconds]" => expiry = Some(part.text().await.unwrap()),
            _ => {}
        }
    }
    assert_eq!(purpose.as_deref(), Some("user_data"));
    assert_eq!(file.as_deref(), Some(b"immutable image bytes".as_slice()));
    assert_eq!(expiry.as_deref(), Some("2592000"));
    let number = state.uploads.fetch_add(1, Ordering::SeqCst) + 1;
    if state.block.load(Ordering::SeqCst) {
        state.gates.acquire().await.unwrap().forget();
    }
    Json(json!({"id":format!("file-{number}"),"expires_at":4_000_000_000_u64})).into_response()
}
async fn info(State(state): State<Arc<ServerState>>, Path(id): Path<String>) -> Response {
    state.probes.fetch_add(1, Ordering::SeqCst);
    if id == "file-missing" {
        StatusCode::NOT_FOUND.into_response()
    } else {
        Json(json!({"id":id})).into_response()
    }
}
async fn complete(State(state): State<Arc<ServerState>>, Json(body): Json<Value>) -> Response {
    let responses = body.get("input").is_some();
    state.calls.lock().unwrap().push(body);
    let reject = if state.reject_forever.load(Ordering::SeqCst) {
        state.reject.load(Ordering::SeqCst)
    } else {
        state.reject.swap(0, Ordering::SeqCst)
    };
    if reject != 0 {
        return (
            StatusCode::from_u16(reject).unwrap(),
            Json(json!({"error":{"message":"request rejected"}})),
        )
            .into_response();
    }
    let data = if responses {
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"seen\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":80}}}}\n\n".to_owned()
    } else {
        "data: {\"choices\":[{\"delta\":{\"content\":\"seen\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"prompt_cache_hit_tokens\":80,\"prompt_cache_miss_tokens\":20}}\n\ndata: [DONE]\n\n".to_owned()
    };
    ([("content-type", "text/event-stream")], data).into_response()
}
fn policy() -> ImagePolicy {
    ImagePolicy {
        enabled: true,
        transfer: Some(ImageTransfer::DeepSeekFiles),
        max_images: 600,
        max_image_bytes: 64 * 1024 * 1024,
        max_total_bytes: 200 * 1024 * 1024,
        max_dimension: 8192,
        many_images_dimension: Some((15, 4096)),
        max_request_bytes: 48 * 1024 * 1024,
        file_lifetime: Duration::from_secs(2_592_000),
    }
}
fn request(id: &ImageId) -> TurnRequest {
    TurnRequest {
        session_id: SessionId::generate(),
        job_id: JobId::generate(),
        history: vec![TurnItem::User {
            content: MessageContent::from_parts(vec![
                ContentPart::Text {
                    text: "before".into(),
                },
                ContentPart::Image {
                    image_id: id.clone(),
                },
                ContentPart::Text {
                    text: "after".into(),
                },
            ]),
        }],
        prefix: AgentPrefix::baseline_chat(Vec::new()),
        tail_state: Some(TailState::text("tail")),
        tool_call_seal: ToolCallSealPolicy::LeaveOpen,
        cancel: TurnCancel::new(),
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition timeout");
}

#[tokio::test]
async fn two_dialects_preserve_content_and_tail_order_and_reuse_one_upload() {
    let server = Server::start().await;
    let source = Arc::new(Source::new());
    let images =
        Arc::new(ProviderImages::new(source.clone(), Arc::new(MemoryCache::default())).unwrap());
    let id = ImageId::generate();
    for style in [ApiStyle::Completions, ApiStyle::Responses] {
        let runtime = OpenAiCompatRuntime::new(server.config(style, "key"))
            .with_images(images.clone(), policy());
        let request = request(&id);
        runtime
            .validate_images(&request.session_id, &request.history)
            .await
            .unwrap();
        let original = request.history.clone();
        let mut stream = runtime.run(request).await.unwrap();
        let mut usage = None;
        while let Some(event) = stream.next().await {
            if let phi_kernel::AgentEvent::Usage { usage: value } = event.unwrap() {
                usage = Some(value);
            }
        }
        assert_eq!(usage.unwrap().cached_tokens, Some(80));
        let TurnItem::User { content } = &original[0] else {
            panic!()
        };
        assert_eq!(content.plain_text(), "beforeafter");
    }
    assert_eq!(server.state.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        1,
        "metadata/cache hit must not read image bytes"
    );
    let calls = server.state.calls.lock().unwrap();
    assert_eq!(
        calls[0]["messages"][0]["content"],
        json!([
        {"type":"text","text":"before"},{"type":"file","file_id":"file-1"},
        {"type":"text","text":"after"},{"type":"text","text":"tail"}])
    );
    assert_eq!(
        calls[1]["input"][0]["content"],
        json!([
        {"type":"input_text","text":"before"},{"type":"input_image","file_id":"file-1"},
        {"type":"input_text","text":"after"},{"type":"input_text","text":"tail"}])
    );
}

#[tokio::test]
async fn cancelling_one_waiter_does_not_cancel_the_other_upload() {
    let server = Server::start().await;
    server.state.block.store(true, Ordering::SeqCst);
    let source = Arc::new(Source::new());
    let images =
        Arc::new(ProviderImages::new(source.clone(), Arc::new(MemoryCache::default())).unwrap());
    let request = request(&ImageId::generate());
    let config = server.config(ApiStyle::Completions, "key");
    let first_cancel = TurnCancel::new();
    let second_cancel = TurnCancel::new();
    let start = |cancel: TurnCancel| {
        let service = images.clone();
        let config = config.clone();
        let request = request.clone();
        tokio::spawn(async move {
            service
                .prepare(
                    &config,
                    &policy(),
                    &request.session_id,
                    &request.history,
                    &cancel,
                )
                .await
        })
    };
    let first = start(first_cancel.clone());
    let second = start(second_cancel);
    until(|| server.state.uploads.load(Ordering::SeqCst) == 1).await;
    // Both callers must have joined the same flight before cancelling one.
    until(|| images.flights_waiters_for_test() == 2).await;
    first_cancel.cancel();
    assert!(first.await.unwrap().is_err());
    server.state.gates.add_permits(1);
    assert!(second.await.unwrap().is_ok());
    assert_eq!(server.state.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn all_waiters_cancel_then_a_new_request_can_upload() {
    let server = Server::start().await;
    server.state.block.store(true, Ordering::SeqCst);
    let images = Arc::new(
        ProviderImages::new(Arc::new(Source::new()), Arc::new(MemoryCache::default())).unwrap(),
    );
    let request = request(&ImageId::generate());
    let config = server.config(ApiStyle::Completions, "key");
    let cancel = TurnCancel::new();
    let service = images.clone();
    let config_first = config.clone();
    let first_request = request.clone();
    let first_cancel = cancel.clone();
    let first = tokio::spawn(async move {
        service
            .prepare(
                &config_first,
                &policy(),
                &first_request.session_id,
                &first_request.history,
                &first_cancel,
            )
            .await
    });
    until(|| server.state.uploads.load(Ordering::SeqCst) == 1).await;
    cancel.cancel();
    assert!(first.await.unwrap().is_err());
    server.state.block.store(false, Ordering::SeqCst);
    server.state.gates.add_permits(1);
    let result = images
        .prepare(
            &config,
            &policy(),
            &request.session_id,
            &request.history,
            &TurnCancel::new(),
        )
        .await
        .unwrap();
    assert_eq!(server.state.uploads.load(Ordering::SeqCst), 2);
    assert!(
        matches!(result.values().next().unwrap(),PreparedImage::File{reference,..} if reference.id.as_str()=="file-2")
    );
}

#[tokio::test]
async fn confirmed_missing_file_is_repaired_once_but_auth_and_rate_errors_are_not_retried() {
    for status in [400, 401, 429] {
        let server = Server::start().await;
        server.state.reject.store(status, Ordering::SeqCst);
        let source = Arc::new(Source::new());
        let cache = Arc::new(MemoryCache::default());
        let config = server.config(ApiStyle::Completions, "key");
        let key = FileCacheKey::new(
            &config.api_base,
            &config.api_key,
            ImageDigest::of(&source.bytes),
        );
        cache
            .remember(
                &key,
                &FileReference {
                    id: ProviderFileId::try_new("file-missing").unwrap(),
                    expires_at: None,
                },
            )
            .unwrap();
        let service = Arc::new(ProviderImages::new(source, cache).unwrap());
        let runtime = OpenAiCompatRuntime::new(config).with_images(service, policy());
        let result = runtime.run(request(&ImageId::generate())).await;
        assert_eq!(result.is_ok(), status == 400);
        assert_eq!(
            server.state.uploads.load(Ordering::SeqCst),
            usize::from(status == 400)
        );
        assert_eq!(
            server.state.probes.load(Ordering::SeqCst),
            usize::from(status == 400)
        );
        assert_eq!(
            server.state.calls.lock().unwrap().len(),
            if status == 400 { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn persistent_rejection_stops_after_one_confirmed_file_repair() {
    let server = Server::start().await;
    server.state.reject.store(400, Ordering::SeqCst);
    server.state.reject_forever.store(true, Ordering::SeqCst);
    let source = Arc::new(Source::new());
    let cache = Arc::new(MemoryCache::default());
    let config = server.config(ApiStyle::Completions, "key");
    let key = FileCacheKey::new(
        &config.api_base,
        &config.api_key,
        ImageDigest::of(&source.bytes),
    );
    cache
        .remember(
            &key,
            &FileReference {
                id: ProviderFileId::try_new("file-missing").unwrap(),
                expires_at: None,
            },
        )
        .unwrap();
    let service = Arc::new(ProviderImages::new(source, cache).unwrap());
    let runtime = OpenAiCompatRuntime::new(config).with_images(service, policy());
    assert!(runtime.run(request(&ImageId::generate())).await.is_err());
    assert_eq!(server.state.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(server.state.probes.load(Ordering::SeqCst), 1);
    assert_eq!(server.state.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn capability_rejection_checks_history_before_any_upload_and_credentials_do_not_share_files()
{
    let server = Server::start().await;
    let source = Arc::new(Source::new());
    let service =
        Arc::new(ProviderImages::new(source.clone(), Arc::new(MemoryCache::default())).unwrap());
    let mut request = request(&ImageId::generate());
    request.history.push(TurnItem::User {
        content: MessageContent::text("follow up"),
    });
    let mut disabled = policy();
    disabled.enabled = false;
    assert!(
        service
            .prepare(
                &server.config(ApiStyle::Completions, "one"),
                &disabled,
                &request.session_id,
                &request.history,
                &request.cancel
            )
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    for key in ["one", "two"] {
        service
            .prepare(
                &server.config(ApiStyle::Completions, key),
                &policy(),
                &request.session_id,
                &request.history,
                &request.cancel,
            )
            .await
            .unwrap();
    }
    assert_eq!(server.state.uploads.load(Ordering::SeqCst), 2);
}
