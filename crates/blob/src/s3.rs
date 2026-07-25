//! S3-compatible `BlobStore` adapter with multipart temporary publication.

use std::{collections::HashMap, pin::Pin, sync::Arc};

use aws_sdk_s3::{
    Client,
    config::{BehaviorVersion, Credentials, Region},
    error::SdkError,
    operation::head_object::HeadObjectError,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart, MetadataDirective},
};
use metric_domain::{
    Timestamp,
    blob::{BlobChecksum, BlobKey, BlobKind, BlobObject},
};
use metric_ports::{
    BlobCapacity, BlobReadSession, BlobScanPage, BlobScanRequest, BlobStore, BlobStoreError,
    BlobWriteSession, PortFuture,
};
use tokio::io::{AsyncRead, AsyncReadExt};

const MINIMUM_PART_BYTES: usize = 5 * 1024 * 1024;
const MAXIMUM_PART_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_PARTS: usize = 10_000;
const CHECKSUM_METADATA: &str = "metric-blake3";
const KIND_METADATA: &str = "metric-kind";
const CREATED_METADATA: &str = "metric-created-ms";

#[derive(Clone)]
pub struct S3BlobConfig {
    pub endpoint: Option<Box<str>>,
    pub region: Box<str>,
    pub bucket: Box<str>,
    pub access_key_id: Box<str>,
    pub secret_access_key: Box<str>,
    pub session_token: Option<Box<str>>,
    pub force_path_style: bool,
    pub part_bytes: usize,
    pub max_object_bytes: u64,
}

impl std::fmt::Debug for S3BlobConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3BlobConfig")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .field("force_path_style", &self.force_path_style)
            .field("part_bytes", &self.part_bytes)
            .field("max_object_bytes", &self.max_object_bytes)
            .finish()
    }
}

#[derive(Clone)]
pub struct S3BlobStore {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    bucket: Box<str>,
    part_bytes: usize,
    max_object_bytes: u64,
}

impl S3BlobStore {
    pub fn new(config: S3BlobConfig) -> Result<Self, BlobStoreError> {
        let valid = !config.region.is_empty()
            && valid_bucket(&config.bucket)
            && !config.access_key_id.is_empty()
            && !config.secret_access_key.is_empty()
            && (MINIMUM_PART_BYTES..=MAXIMUM_PART_BYTES).contains(&config.part_bytes)
            && config.max_object_bytes > 0
            && config.max_object_bytes
                <= (config.part_bytes as u64).saturating_mul(MAXIMUM_PARTS as u64);
        if !valid {
            return Err(BlobStoreError::Invalid);
        }
        if config.endpoint.as_deref().is_some_and(|endpoint| {
            !endpoint.starts_with("http://") && !endpoint.starts_with("https://")
        }) {
            return Err(BlobStoreError::Invalid);
        }
        let credentials = Credentials::new(
            config.access_key_id.into_string(),
            config.secret_access_key.into_string(),
            config.session_token.map(|token| token.into_string()),
            None,
            "metric-config",
        );
        let mut builder = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region.into_string()))
            .credentials_provider(credentials)
            .force_path_style(config.force_path_style);
        if let Some(endpoint) = config.endpoint {
            builder = builder.endpoint_url(endpoint.into_string());
        }
        Ok(Self {
            inner: Arc::new(Inner {
                client: Client::from_conf(builder.build()),
                bucket: config.bucket,
                part_bytes: config.part_bytes,
                max_object_bytes: config.max_object_bytes,
            }),
        })
    }

    async fn head(&self, key: &str) -> Result<Option<RemoteObject>, BlobStoreError> {
        match self
            .inner
            .client
            .head_object()
            .bucket(self.inner.bucket.as_ref())
            .key(key)
            .send()
            .await
        {
            Ok(output) => {
                let size = u64::try_from(output.content_length().unwrap_or_default())
                    .map_err(|_| BlobStoreError::Corrupt)?;
                Ok(Some(RemoteObject {
                    size,
                    metadata: output.metadata().cloned().unwrap_or_default(),
                }))
            }
            Err(error) if head_not_found(&error) => Ok(None),
            Err(_) => Err(BlobStoreError::Unavailable),
        }
    }

    async fn delete_raw(&self, key: &str) -> Result<(), BlobStoreError> {
        self.inner
            .client
            .delete_object()
            .bucket(self.inner.bucket.as_ref())
            .key(key)
            .send()
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        Ok(())
    }

    async fn verify_bytes(
        &self,
        key: &str,
        expected_size: u64,
        expected_checksum: BlobChecksum,
    ) -> Result<(), BlobStoreError> {
        let output = self
            .inner
            .client
            .get_object()
            .bucket(self.inner.bucket.as_ref())
            .key(key)
            .send()
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        let mut reader = output.body.into_async_read();
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(u64::try_from(count).map_err(|_| BlobStoreError::Corrupt)?)
                .ok_or(BlobStoreError::Corrupt)?;
            if size > expected_size {
                return Err(BlobStoreError::Corrupt);
            }
            hasher.update(&buffer[..count]);
        }
        let checksum = BlobChecksum::from_bytes(*hasher.finalize().as_bytes());
        if size != expected_size || checksum != expected_checksum {
            return Err(BlobStoreError::Corrupt);
        }
        Ok(())
    }

    async fn published_matches(
        &self,
        key: &str,
        kind: BlobKind,
        size: u64,
        checksum: BlobChecksum,
        created_at: Timestamp,
    ) -> Result<bool, BlobStoreError> {
        let Some(remote) = self.head(key).await? else {
            return Ok(false);
        };
        let matches = remote.size == size
            && remote.metadata.get(CHECKSUM_METADATA).map(String::as_str)
                == Some(checksum.to_string().as_str())
            && remote.metadata.get(KIND_METADATA).map(String::as_str) == Some(kind.name())
            && remote.metadata.get(CREATED_METADATA).map(String::as_str)
                == Some(created_at.unix_millis().to_string().as_str());
        if !matches {
            return Err(BlobStoreError::Corrupt);
        }
        self.verify_bytes(key, size, checksum).await?;
        Ok(true)
    }
}

impl BlobStore for S3BlobStore {
    fn begin(
        &self,
        kind: BlobKind,
        created_at: Timestamp,
    ) -> PortFuture<'_, Result<Box<dyn BlobWriteSession>, BlobStoreError>> {
        let store = self.clone();
        Box::pin(async move {
            Ok(Box::new(S3WriteSession {
                temporary_key: format!("metric-temporary/{}", uuid::Uuid::new_v4()).into(),
                store,
                kind,
                created_at,
                size: 0,
                checksum: blake3::Hasher::new(),
                buffer: Vec::new(),
                upload_id: None,
                parts: Vec::new(),
                completed: false,
            }) as Box<dyn BlobWriteSession>)
        })
    }

    fn open(
        &self,
        key: &BlobKey,
    ) -> PortFuture<'_, Result<Box<dyn BlobReadSession>, BlobStoreError>> {
        let store = self.clone();
        let key = key.as_str().to_owned();
        Box::pin(async move {
            if store.head(&key).await?.is_none() {
                return Err(BlobStoreError::NotFound);
            }
            let output = store
                .inner
                .client
                .get_object()
                .bucket(store.inner.bucket.as_ref())
                .key(key)
                .send()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            Ok(Box::new(S3ReadSession {
                reader: Box::pin(output.body.into_async_read()),
            }) as Box<dyn BlobReadSession>)
        })
    }

    fn delete(&self, key: &BlobKey) -> PortFuture<'_, Result<(), BlobStoreError>> {
        let store = self.clone();
        let key = key.as_str().to_owned();
        Box::pin(async move { store.delete_raw(&key).await })
    }

    fn scan(
        &self,
        request: BlobScanRequest,
    ) -> PortFuture<'_, Result<BlobScanPage, BlobStoreError>> {
        let store = self.clone();
        Box::pin(async move {
            if request.limit == 0 || request.limit > 10_000 {
                return Err(BlobStoreError::Invalid);
            }
            let output = store
                .inner
                .client
                .list_objects_v2()
                .bucket(store.inner.bucket.as_ref())
                .prefix(request.namespace.prefix())
                .set_start_after(request.cursor.as_deref().map(str::to_owned))
                .max_keys(i32::try_from(request.limit).map_err(|_| BlobStoreError::Invalid)?)
                .send()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            let mut objects = Vec::with_capacity(output.contents().len());
            let mut last_examined = None;
            for listed in output.contents() {
                let Some(key_text) = listed.key() else {
                    return Err(BlobStoreError::Corrupt);
                };
                last_examined = Some(key_text.to_owned().into_boxed_str());
                let key = BlobKey::new(key_text.to_owned()).map_err(|_| BlobStoreError::Corrupt)?;
                let Ok(kind) = request.namespace.kind_for_key(&key) else {
                    continue;
                };
                let Some(remote) = store.head(key_text).await? else {
                    continue;
                };
                let Some(created_at) = remote
                    .metadata
                    .get(CREATED_METADATA)
                    .and_then(|value| value.parse::<i64>().ok())
                    .and_then(|value| Timestamp::from_unix_millis(value).ok())
                else {
                    return Err(BlobStoreError::Corrupt);
                };
                if created_at > request.older_than {
                    continue;
                }
                let checksum = parse_checksum(
                    remote
                        .metadata
                        .get(CHECKSUM_METADATA)
                        .ok_or(BlobStoreError::Corrupt)?,
                )?;
                if remote.metadata.get(KIND_METADATA).map(String::as_str) != Some(kind.name()) {
                    return Err(BlobStoreError::Corrupt);
                }
                objects.push(BlobObject {
                    key,
                    kind,
                    size: remote.size,
                    checksum,
                    created_at,
                });
            }
            let next_cursor = output
                .is_truncated()
                .unwrap_or(false)
                .then_some(last_examined)
                .flatten();
            Ok(BlobScanPage {
                objects,
                next_cursor,
            })
        })
    }

    fn capacity(&self) -> BlobCapacity {
        BlobCapacity {
            used_bytes: 0,
            writable_bytes: u64::MAX,
            reserve_bytes: 0,
        }
    }
}

struct S3WriteSession {
    store: S3BlobStore,
    temporary_key: Box<str>,
    kind: BlobKind,
    created_at: Timestamp,
    size: u64,
    checksum: blake3::Hasher,
    buffer: Vec<u8>,
    upload_id: Option<Box<str>>,
    parts: Vec<CompletedPart>,
    completed: bool,
}

impl S3WriteSession {
    async fn ensure_multipart(&mut self) -> Result<&str, BlobStoreError> {
        if self.upload_id.is_none() {
            let output = self
                .store
                .inner
                .client
                .create_multipart_upload()
                .bucket(self.store.inner.bucket.as_ref())
                .key(self.temporary_key.as_ref())
                .send()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            self.upload_id = Some(
                output
                    .upload_id()
                    .ok_or(BlobStoreError::Corrupt)?
                    .to_owned()
                    .into_boxed_str(),
            );
        }
        Ok(self
            .upload_id
            .as_deref()
            .expect("multipart upload ID was installed"))
    }

    async fn flush_part(&mut self) -> Result<(), BlobStoreError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        if self.parts.len() >= MAXIMUM_PARTS {
            return Err(BlobStoreError::TooLarge);
        }
        let upload_id = self.ensure_multipart().await?.to_owned();
        let bytes = std::mem::take(&mut self.buffer);
        let part_number =
            i32::try_from(self.parts.len() + 1).map_err(|_| BlobStoreError::TooLarge)?;
        let mut output = None;
        for attempt in 0..3_u32 {
            match self
                .store
                .inner
                .client
                .upload_part()
                .bucket(self.store.inner.bucket.as_ref())
                .key(self.temporary_key.as_ref())
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(ByteStream::from(bytes.clone()))
                .send()
                .await
            {
                Ok(result) => {
                    output = Some(result);
                    break;
                }
                Err(_) if attempt < 2 => {
                    tokio::time::sleep(std::time::Duration::from_millis(10_u64 << attempt)).await;
                }
                Err(_) => return Err(BlobStoreError::Unavailable),
            }
        }
        let output = output.ok_or(BlobStoreError::Unavailable)?;
        let e_tag = output.e_tag().ok_or(BlobStoreError::Corrupt)?.to_owned();
        self.parts.push(
            CompletedPart::builder()
                .part_number(part_number)
                .e_tag(e_tag)
                .build(),
        );
        Ok(())
    }

    async fn complete_temporary(&mut self) -> Result<(), BlobStoreError> {
        if self.upload_id.is_none() && self.buffer.len() < self.store.inner.part_bytes {
            let bytes = std::mem::take(&mut self.buffer);
            self.store
                .inner
                .client
                .put_object()
                .bucket(self.store.inner.bucket.as_ref())
                .key(self.temporary_key.as_ref())
                .body(ByteStream::from(bytes))
                .send()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            return Ok(());
        }
        self.flush_part().await?;
        let upload_id = self.upload_id.take().ok_or(BlobStoreError::Unavailable)?;
        let upload = CompletedMultipartUpload::builder()
            .set_parts(Some(std::mem::take(&mut self.parts)))
            .build();
        self.store
            .inner
            .client
            .complete_multipart_upload()
            .bucket(self.store.inner.bucket.as_ref())
            .key(self.temporary_key.as_ref())
            .upload_id(upload_id.as_ref())
            .multipart_upload(upload)
            .send()
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        Ok(())
    }

    async fn abort_inner(&mut self) -> Result<(), BlobStoreError> {
        if let Some(upload_id) = self.upload_id.take() {
            self.store
                .inner
                .client
                .abort_multipart_upload()
                .bucket(self.store.inner.bucket.as_ref())
                .key(self.temporary_key.as_ref())
                .upload_id(upload_id.as_ref())
                .send()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
        } else {
            self.store.delete_raw(self.temporary_key.as_ref()).await?;
        }
        self.completed = true;
        Ok(())
    }
}

impl BlobWriteSession for S3WriteSession {
    fn write_chunk(&mut self, chunk: Box<[u8]>) -> PortFuture<'_, Result<(), BlobStoreError>> {
        Box::pin(async move {
            let length = u64::try_from(chunk.len()).map_err(|_| BlobStoreError::TooLarge)?;
            let next = self
                .size
                .checked_add(length)
                .ok_or(BlobStoreError::TooLarge)?;
            if next > self.store.inner.max_object_bytes {
                return Err(BlobStoreError::TooLarge);
            }
            self.checksum.update(&chunk);
            self.size = next;
            self.buffer.extend_from_slice(&chunk);
            while self.buffer.len() >= self.store.inner.part_bytes {
                let remainder = self.buffer.split_off(self.store.inner.part_bytes);
                self.flush_part().await?;
                self.buffer = remainder;
            }
            Ok(())
        })
    }

    fn commit(
        mut self: Box<Self>,
        key: BlobKey,
    ) -> PortFuture<'static, Result<BlobObject, BlobStoreError>> {
        Box::pin(async move {
            let checksum = BlobChecksum::from_bytes(*self.checksum.clone().finalize().as_bytes());
            if self
                .store
                .published_matches(
                    key.as_str(),
                    self.kind,
                    self.size,
                    checksum,
                    self.created_at,
                )
                .await?
            {
                self.abort_inner().await?;
                return Ok(BlobObject {
                    key,
                    kind: self.kind,
                    size: self.size,
                    checksum,
                    created_at: self.created_at,
                });
            }
            self.complete_temporary().await?;
            let copy_source = format!(
                "{}/{}",
                self.store.inner.bucket.as_ref(),
                self.temporary_key.as_ref()
            );
            self.store
                .inner
                .client
                .copy_object()
                .bucket(self.store.inner.bucket.as_ref())
                .key(key.as_str())
                .copy_source(copy_source)
                .metadata_directive(MetadataDirective::Replace)
                .metadata(CHECKSUM_METADATA, checksum.to_string())
                .metadata(KIND_METADATA, self.kind.name())
                .metadata(CREATED_METADATA, self.created_at.unix_millis().to_string())
                .send()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            if !self
                .store
                .published_matches(
                    key.as_str(),
                    self.kind,
                    self.size,
                    checksum,
                    self.created_at,
                )
                .await?
            {
                return Err(BlobStoreError::Corrupt);
            }
            self.store.delete_raw(self.temporary_key.as_ref()).await?;
            self.completed = true;
            Ok(BlobObject {
                key,
                kind: self.kind,
                size: self.size,
                checksum,
                created_at: self.created_at,
            })
        })
    }

    fn abort(mut self: Box<Self>) -> PortFuture<'static, Result<(), BlobStoreError>> {
        Box::pin(async move { self.abort_inner().await })
    }
}

impl Drop for S3WriteSession {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let store = self.store.clone();
        let key = self.temporary_key.clone();
        let upload_id = self.upload_id.take();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Some(upload_id) = upload_id {
                    let _ = store
                        .inner
                        .client
                        .abort_multipart_upload()
                        .bucket(store.inner.bucket.as_ref())
                        .key(key.as_ref())
                        .upload_id(upload_id.as_ref())
                        .send()
                        .await;
                } else {
                    let _ = store.delete_raw(key.as_ref()).await;
                }
            });
        }
    }
}

struct S3ReadSession {
    reader: Pin<Box<dyn AsyncRead + Send>>,
}

impl BlobReadSession for S3ReadSession {
    fn read_chunk(
        &mut self,
        maximum: usize,
    ) -> PortFuture<'_, Result<Option<Box<[u8]>>, BlobStoreError>> {
        Box::pin(async move {
            if maximum == 0 || maximum > 1024 * 1024 {
                return Err(BlobStoreError::Invalid);
            }
            let mut bytes = vec![0_u8; maximum];
            let count = self
                .reader
                .read(&mut bytes)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            if count == 0 {
                return Ok(None);
            }
            bytes.truncate(count);
            Ok(Some(bytes.into_boxed_slice()))
        })
    }
}

struct RemoteObject {
    size: u64,
    metadata: HashMap<String, String>,
}

fn valid_bucket(value: &str) -> bool {
    (3..=63).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && value
            .as_bytes()
            .first()
            .zip(value.as_bytes().last())
            .is_some_and(|(first, last)| {
                first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
            })
}

fn head_not_found<R>(error: &SdkError<HeadObjectError, R>) -> bool {
    error
        .as_service_error()
        .is_some_and(HeadObjectError::is_not_found)
}

fn parse_checksum(value: &str) -> Result<BlobChecksum, BlobStoreError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BlobStoreError::Corrupt);
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| BlobStoreError::Corrupt)?;
    Ok(BlobChecksum::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_redacts_credentials_and_rejects_small_parts() {
        let config = S3BlobConfig {
            endpoint: Some("http://127.0.0.1:9000".into()),
            region: "us-east-1".into(),
            bucket: "metric-test".into(),
            access_key_id: "AKIA_PHASE21_VALUE".into(),
            secret_access_key: "phase21-private-value".into(),
            session_token: Some("phase21-session-value".into()),
            force_path_style: true,
            part_bytes: MINIMUM_PART_BYTES,
            max_object_bytes: 10 * 1024 * 1024,
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("AKIA_PHASE21_VALUE"));
        assert!(!rendered.contains("phase21-private-value"));
        assert!(!rendered.contains("phase21-session-value"));
        let mut invalid = config;
        invalid.part_bytes = MINIMUM_PART_BYTES - 1;
        assert!(matches!(
            S3BlobStore::new(invalid),
            Err(BlobStoreError::Invalid)
        ));
    }
}
