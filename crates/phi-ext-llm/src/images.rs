//! Image bytes are resolved by the host; provider files are a rebuildable cache.
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use bytes::Bytes;
use phi_kernel::{ContentPart, ImageId, SessionId, TurnCancel, TurnItem};
use reqwest::{Client, multipart};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::{ApiBase, ApiKey, HttpConnection};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageDigest(String);
impl ImageDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }
    pub fn try_new(value: &str) -> Result<Self, String> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid image digest".into());
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct ImageMetadata {
    pub digest: ImageDigest,
    pub mime_type: String,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
}

/// Metadata must describe immutable bytes. Cache hits never call `read`.
#[async_trait]
pub trait ImageSource: Send + Sync {
    async fn metadata(&self, session: &SessionId, image: &ImageId)
    -> Result<ImageMetadata, String>;
    async fn read(&self, session: &SessionId, image: &ImageId) -> Result<Bytes, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageTransfer {
    Inline,
    DeepSeekFiles,
}

/// Host-selected request policy. The adapter applies it to the whole projection.
#[derive(Clone, Debug)]
pub struct ImagePolicy {
    pub enabled: bool,
    pub transfer: Option<ImageTransfer>,
    pub max_images: usize,
    pub max_image_bytes: u64,
    pub max_total_bytes: u64,
    pub max_dimension: u32,
    pub many_images_dimension: Option<(usize, u32)>,
    pub max_request_bytes: usize,
    pub file_lifetime: Duration,
}

#[derive(Clone, Eq, PartialEq, Hash)]
pub struct FileCacheKey {
    scope: String,
    digest: ImageDigest,
}
impl FileCacheKey {
    pub fn new(base: &ApiBase, key: &ApiKey, digest: ImageDigest) -> Self {
        // Length-separated components; the cache never contains a bearer secret.
        let mut hasher = Sha256::new();
        hasher.update(base.as_str().len().to_le_bytes());
        hasher.update(base.as_str().as_bytes());
        hasher.update(key.as_str().as_bytes());
        Self {
            scope: format!("{:x}", hasher.finalize()),
            digest,
        }
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn digest(&self) -> &ImageDigest {
        &self.digest
    }
}
impl fmt::Debug for FileCacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileCacheKey")
            .field("scope", &"[redacted]")
            .field("digest", &self.digest)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFileId(String);
impl ProviderFileId {
    pub fn try_new(value: &str) -> Result<Self, String> {
        if value.is_empty()
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid provider file id".into());
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct FileReference {
    pub id: ProviderFileId,
    pub expires_at: Option<u64>,
}
impl FileReference {
    pub fn is_current(&self) -> bool {
        self.expires_at
            .is_none_or(|expiry| expiry > Self::now().saturating_add(60))
    }
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

/// Persistence belongs to the host. All operations are short local transactions.
pub trait FileReferenceCache: Send + Sync {
    fn lookup(&self, key: &FileCacheKey) -> Result<Option<FileReference>, String>;
    fn remember(&self, key: &FileCacheKey, value: &FileReference) -> Result<(), String>;
    /// Compare-and-remove prevents a late failure from removing a repaired entry.
    fn forget(&self, key: &FileCacheKey, id: &ProviderFileId) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub enum PreparedImage {
    Inline {
        mime_type: String,
        bytes: Bytes,
    },
    File {
        key: FileCacheKey,
        reference: FileReference,
    },
}
#[derive(Clone, Debug, Default)]
pub struct PreparedImages {
    entries: HashMap<ImageId, PreparedImage>,
    max_request_bytes: Option<usize>,
    tool_materials: HashMap<usize, phi_kernel::MessageContent>,
}
impl PreparedImages {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, id: &ImageId) -> Option<&PreparedImage> {
        self.entries.get(id)
    }
    pub fn insert(&mut self, id: ImageId, image: PreparedImage) -> Option<PreparedImage> {
        self.entries.insert(id, image)
    }
    pub fn contains_key(&self, id: &ImageId) -> bool {
        self.entries.contains_key(id)
    }
    pub fn values(&self) -> impl Iterator<Item = &PreparedImage> {
        self.entries.values()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&ImageId, &PreparedImage)> {
        self.entries.iter()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// A request-local history position, not a provider call identity (which can repeat).
    pub fn tool_output_at(&self, position: usize) -> Option<&phi_kernel::MessageContent> {
        self.tool_materials.get(&position)
    }
    pub(crate) fn attach_tool_outputs(
        &mut self,
        materials: HashMap<usize, phi_kernel::MessageContent>,
    ) {
        self.tool_materials = materials;
    }
    pub fn validate_request_bytes(&self, len: usize) -> Result<(), String> {
        if self.max_request_bytes.is_some_and(|max| len > max) {
            Err("请求体超出 Provider 限制，请减少当前上下文的图片或文字".into())
        } else {
            Ok(())
        }
    }
}

struct UploadFlight {
    result: watch::Sender<Option<Result<FileReference, String>>>,
    waiters: AtomicUsize,
    cancel: TurnCancel,
}
impl UploadFlight {
    fn try_join(&self) -> bool {
        // Zero is terminal: a new waiter must never revive a flight whose last
        // owner is between decrementing the count and signaling cancellation.
        self.waiters
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                if count == 0 {
                    None
                } else {
                    count.checked_add(1)
                }
            })
            .is_ok()
    }
}
struct UploadWaiter(Arc<UploadFlight>);
impl Drop for UploadWaiter {
    fn drop(&mut self) {
        if self.0.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.cancel.cancel();
        }
    }
}

/// Shared across runtimes and models. One upload serves independently cancellable callers.
pub struct ProviderImages {
    source: Arc<dyn ImageSource>,
    cache: Arc<dyn FileReferenceCache>,
    client: Client,
    flights: Mutex<HashMap<FileCacheKey, Weak<UploadFlight>>>,
}
impl ProviderImages {
    #[cfg(test)]
    pub(crate) fn flights_waiters_for_test(&self) -> usize {
        self.flights
            .lock()
            .unwrap()
            .values()
            .filter_map(Weak::upgrade)
            .map(|flight| flight.waiters.load(Ordering::Acquire))
            .sum()
    }
    pub fn new(
        source: Arc<dyn ImageSource>,
        cache: Arc<dyn FileReferenceCache>,
    ) -> Result<Self, String> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            source,
            cache,
            client,
            flights: Mutex::new(HashMap::new()),
        })
    }

    async fn inspect(
        &self,
        policy: &ImagePolicy,
        session: &SessionId,
        history: &[TurnItem],
        cancel: &TurnCancel,
    ) -> Result<(Vec<ImageId>, HashMap<ImageId, ImageMetadata>), String> {
        let mut occurrences = Vec::new();
        for item in history {
            if let TurnItem::User { content } = item {
                for part in content.parts() {
                    if let ContentPart::Image { image_id } = part {
                        occurrences.push(image_id.clone());
                    }
                }
            }
        }
        if occurrences.is_empty() {
            return Ok((occurrences, HashMap::new()));
        }
        if !policy.enabled {
            return Err(
                "所选模型不支持图片，请选择支持图片的模型（历史消息中也可能有图片）".into(),
            );
        }
        policy
            .transfer
            .ok_or("请在 Provider 设置中明确选择图片传输方式")?;
        if occurrences.len() > policy.max_images {
            return Err(format!(
                "当前上下文包含 {} 张图片，超过 {} 张限制",
                occurrences.len(),
                policy.max_images
            ));
        }
        let max_dimension = policy
            .many_images_dimension
            .filter(|(count, _)| occurrences.len() >= *count)
            .map_or(policy.max_dimension, |(_, dimension)| dimension);
        let mut metadata = HashMap::new();
        let mut total = 0_u64;
        for id in &occurrences {
            if !metadata.contains_key(id) {
                let value = tokio::select! {
                    () = cancel.cancelled() => return Err("image preparation cancelled".into()),
                    result = self.source.metadata(session, id) => result?,
                };
                if !matches!(
                    value.mime_type.as_str(),
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                ) {
                    return Err("unsupported image content type".into());
                }
                if value.byte_len == 0
                    || value.byte_len > policy.max_image_bytes
                    || value.width == 0
                    || value.height == 0
                    || value.width > max_dimension
                    || value.height > max_dimension
                {
                    return Err(format!(
                        "图片超出当前请求限制：单张最多 {} MiB，最长边 {} px",
                        policy.max_image_bytes / 1024 / 1024,
                        max_dimension
                    ));
                }
                metadata.insert(id.clone(), value);
            }
            total = total
                .checked_add(metadata[id].byte_len)
                .ok_or("image size overflow")?;
        }
        if total > policy.max_total_bytes {
            return Err("当前上下文图片总大小超出 Provider 限制".into());
        }
        if policy.transfer == Some(ImageTransfer::Inline)
            && total.saturating_add(2) / 3 * 4 > policy.max_request_bytes as u64
        {
            return Err("图片的内联编码超过请求体限制，请减少图片或配置 Files 传输".into());
        }
        Ok((occurrences, metadata))
    }

    pub(crate) async fn validate(
        &self,
        policy: &ImagePolicy,
        session: &SessionId,
        history: &[TurnItem],
    ) -> Result<(), String> {
        self.inspect(policy, session, history, &TurnCancel::new())
            .await
            .map(|_| ())
    }

    pub(crate) async fn prepare(
        self: &Arc<Self>,
        config: &HttpConnection,
        policy: &ImagePolicy,
        session: &SessionId,
        history: &[TurnItem],
        cancel: &TurnCancel,
    ) -> Result<PreparedImages, String> {
        let (occurrences, mut metadata) = self.inspect(policy, session, history, cancel).await?;
        if occurrences.is_empty() {
            return Ok(PreparedImages {
                max_request_bytes: Some(policy.max_request_bytes),
                ..PreparedImages::new()
            });
        }
        let transfer = policy.transfer.ok_or("image transfer not configured")?;
        // Validate the entire request before any upload starts.
        let mut prepared = PreparedImages {
            max_request_bytes: Some(policy.max_request_bytes),
            ..PreparedImages::new()
        };
        for id in occurrences {
            if prepared.contains_key(&id) {
                continue;
            }
            let meta = metadata.remove(&id).expect("validated metadata");
            let image = match transfer {
                ImageTransfer::Inline => {
                    let bytes = tokio::select! {
                        () = cancel.cancelled() => return Err("image preparation cancelled".into()),
                        result = self.read_verified(session, &id, &meta) => result?,
                    };
                    PreparedImage::Inline {
                        mime_type: meta.mime_type,
                        bytes,
                    }
                }
                ImageTransfer::DeepSeekFiles => {
                    let key =
                        FileCacheKey::new(&config.api_base, &config.api_key, meta.digest.clone());
                    let reference = self
                        .reference(config, policy, session, &id, meta, &key, cancel)
                        .await?;
                    PreparedImage::File { key, reference }
                }
            };
            prepared.insert(id, image);
        }
        Ok(prepared)
    }

    async fn read_verified(
        &self,
        session: &SessionId,
        id: &ImageId,
        metadata: &ImageMetadata,
    ) -> Result<Bytes, String> {
        let bytes = self.source.read(session, id).await?;
        if bytes.len() as u64 != metadata.byte_len || ImageDigest::of(&bytes) != metadata.digest {
            return Err("stored image does not match its immutable metadata".into());
        }
        Ok(bytes)
    }

    async fn reference(
        self: &Arc<Self>,
        config: &HttpConnection,
        policy: &ImagePolicy,
        session: &SessionId,
        id: &ImageId,
        meta: ImageMetadata,
        key: &FileCacheKey,
        cancel: &TurnCancel,
    ) -> Result<FileReference, String> {
        if let Some(value) = self.cache.lookup(key)?.filter(FileReference::is_current) {
            return Ok(value);
        }
        let (flight, start) = {
            let mut flights = self
                .flights
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            flights.retain(|_, value| value.strong_count() > 0);
            if let Some(flight) = flights
                .get(key)
                .and_then(Weak::upgrade)
                .filter(|f| f.try_join())
            {
                (flight, false)
            } else {
                let (result, _) = watch::channel(None);
                let flight = Arc::new(UploadFlight {
                    result,
                    waiters: AtomicUsize::new(1),
                    cancel: TurnCancel::new(),
                });
                flights.insert(key.clone(), Arc::downgrade(&flight));
                (flight, true)
            }
        };
        let _waiter = UploadWaiter(flight.clone());
        if start {
            let service = self.clone();
            let flight = flight.clone();
            let config = config.clone();
            let key = key.clone();
            let session = session.clone();
            let id = id.clone();
            let lifetime = policy.file_lifetime;
            tokio::spawn(async move {
                let result = tokio::select! {
                    () = flight.cancel.cancelled() => Err("image upload cancelled".into()),
                    result = service.upload(&config, &session, &id, &meta, &key, lifetime) => result,
                };
                flight.result.send_replace(Some(result));
            });
        }
        let mut result = flight.result.subscribe();
        loop {
            if let Some(value) = result.borrow().clone() {
                return value;
            }
            tokio::select! {
                () = cancel.cancelled() => return Err("image upload cancelled".into()),
                changed = result.changed() => changed.map_err(|_| "image upload interrupted")?,
            }
        }
    }

    async fn upload(
        &self,
        config: &HttpConnection,
        session: &SessionId,
        id: &ImageId,
        meta: &ImageMetadata,
        key: &FileCacheKey,
        lifetime: Duration,
    ) -> Result<FileReference, String> {
        // Another flight can finish between the caller's lookup and acquiring the slot.
        if let Some(value) = self.cache.lookup(key)?.filter(FileReference::is_current) {
            return Ok(value);
        }
        let seconds = lifetime.as_secs();
        if !(3600..=2_592_000).contains(&seconds) {
            return Err("file lifetime must be 1 hour to 30 days".into());
        }
        let bytes = self.read_verified(session, id, meta).await?;
        let extension = match meta.mime_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            _ => "webp",
        };
        let part = multipart::Part::stream(reqwest::Body::from(bytes))
            .file_name(format!("image.{extension}"))
            .mime_str(&meta.mime_type)
            .map_err(|e| e.to_string())?;
        let form = multipart::Form::new()
            .text("purpose", "user_data")
            .text("expires_after[anchor]", "created_at")
            .text("expires_after[seconds]", seconds.to_string())
            .part("file", part);
        let response = config
            .authorize(self.client.post(format!("{}/files", config.api_base)))
            .multipart(form)
            .send()
            .await
            .map_err(|_| "图片上传失败，请检查网络后重试".to_string())?;
        if !response.status().is_success() {
            return Err(format!("图片上传失败（HTTP {}）", response.status()));
        }
        #[derive(Deserialize)]
        struct Uploaded {
            id: String,
            expires_at: Option<u64>,
        }
        let uploaded: Uploaded = response
            .json()
            .await
            .map_err(|_| "Provider 返回的图片文件信息无效")?;
        let reference = FileReference {
            id: ProviderFileId::try_new(&uploaded.id)?,
            expires_at: Some(
                uploaded
                    .expires_at
                    .unwrap_or_else(|| FileReference::now().saturating_add(seconds)),
            ),
        };
        self.cache.remember(key, &reference)?;
        Ok(reference)
    }

    /// A rejected request alone does not prove expiry. Only GET /files/{id} = 404 repairs a reference.
    pub(crate) async fn repair_missing(
        &self,
        config: &HttpConnection,
        prepared: &PreparedImages,
        cancel: &TurnCancel,
    ) -> Result<bool, String> {
        let mut missing = Vec::new();
        for image in prepared.values() {
            if let PreparedImage::File { key, reference } = image {
                let response = tokio::select! {
                    () = cancel.cancelled() => return Err("image verification cancelled".into()),
                    result = config.authorize(self.client.get(format!("{}/files/{}", config.api_base, reference.id.as_str())))
                        .timeout(Duration::from_secs(20)).send() => result.map_err(|_| "unable to verify provider file")?,
                };
                match response.status().as_u16() {
                    404 => missing.push((key, &reference.id)),
                    200 => {}
                    _ => return Ok(false),
                }
            }
        }
        for (key, id) in &missing {
            self.cache.forget(key, id)?;
        }
        Ok(!missing.is_empty())
    }
}

#[cfg(test)]
mod flight_tests {
    use super::*;

    #[test]
    fn joining_and_last_waiter_departure_are_linearized() {
        for _ in 0..64 {
            let (result, _) = watch::channel(None);
            let flight = Arc::new(UploadFlight {
                result,
                waiters: AtomicUsize::new(1),
                cancel: TurnCancel::new(),
            });
            let previous = UploadWaiter(flight.clone());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let departing = barrier.clone();
            let thread = std::thread::spawn(move || {
                departing.wait();
                drop(previous);
            });
            barrier.wait();
            let joined = flight.try_join();
            thread.join().unwrap();
            if joined {
                assert!(
                    !flight.cancel.is_cancelled(),
                    "a newly joined waiter retains the upload"
                );
                assert_eq!(flight.waiters.load(Ordering::Acquire), 1);
                drop(UploadWaiter(flight));
            } else {
                assert!(flight.cancel.is_cancelled());
                assert_eq!(flight.waiters.load(Ordering::Acquire), 0);
            }
        }
    }
}
