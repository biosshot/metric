//! Local filesystem BlobStore with atomic temporary-to-final publication.

mod s3;

pub use s3::{S3BlobConfig, S3BlobStore};

use std::{
    collections::BinaryHeap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::UNIX_EPOCH,
};

use faultkeep_domain::{
    Timestamp,
    blob::{BlobChecksum, BlobKey, BlobKind, BlobObject},
};
use faultkeep_ports::{
    BlobCapacity, BlobReadSession, BlobScanPage, BlobScanRequest, BlobStore, BlobStoreError,
    BlobWriteSession, PortFuture,
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

#[derive(Debug, Clone, Copy)]
pub struct LocalBlobConfig {
    pub capacity_bytes: u64,
    pub reserve_bytes: u64,
    pub max_object_bytes: u64,
}

impl Default for LocalBlobConfig {
    fn default() -> Self {
        Self {
            capacity_bytes: 100 * 1024 * 1024 * 1024,
            reserve_bytes: 5 * 1024 * 1024 * 1024,
            max_object_bytes: 100 * 1024 * 1024,
        }
    }
}

#[derive(Clone)]
pub struct LocalBlobStore {
    inner: Arc<Inner>,
}

struct Inner {
    objects: PathBuf,
    temporary: PathBuf,
    used_bytes: AtomicU64,
    writable_bytes: u64,
    reserve_bytes: u64,
    max_object_bytes: u64,
}

impl LocalBlobStore {
    pub async fn new(
        root: impl AsRef<Path>,
        config: LocalBlobConfig,
    ) -> Result<Self, BlobStoreError> {
        if config.capacity_bytes == 0
            || config.reserve_bytes >= config.capacity_bytes
            || config.max_object_bytes == 0
            || config.max_object_bytes > config.capacity_bytes - config.reserve_bytes
        {
            return Err(BlobStoreError::Invalid);
        }
        let root = absolute(root.as_ref())?;
        let objects = root.join("objects");
        let temporary = root.join("temporary");
        fs::create_dir_all(&objects)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        fs::create_dir_all(&temporary)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        let objects = fs::canonicalize(objects)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        let temporary = fs::canonicalize(temporary)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        cleanup_temporary(&temporary).await?;
        let used_bytes =
            directory_bytes(objects.clone()).await? + directory_bytes(temporary.clone()).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                objects,
                temporary,
                used_bytes: AtomicU64::new(used_bytes),
                writable_bytes: config.capacity_bytes - config.reserve_bytes,
                reserve_bytes: config.reserve_bytes,
                max_object_bytes: config.max_object_bytes,
            }),
        })
    }

    fn final_path(&self, key: &BlobKey) -> Result<PathBuf, BlobStoreError> {
        let path = self.inner.objects.join(key.as_str());
        path.starts_with(&self.inner.objects)
            .then_some(path)
            .ok_or(BlobStoreError::Invalid)
    }
}

async fn cleanup_temporary(directory: &Path) -> Result<(), BlobStoreError> {
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|_| BlobStoreError::Unavailable)?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| BlobStoreError::Unavailable)?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        let is_part = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".part"));
        if !file_type.is_file() || file_type.is_symlink() || !is_part {
            return Err(BlobStoreError::Invalid);
        }
        fs::remove_file(entry.path())
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
    }
    Ok(())
}

impl BlobStore for LocalBlobStore {
    fn begin(
        &self,
        kind: BlobKind,
        created_at: Timestamp,
    ) -> PortFuture<'_, Result<Box<dyn BlobWriteSession>, BlobStoreError>> {
        let store = self.clone();
        Box::pin(async move {
            let path = store
                .inner
                .temporary
                .join(format!("{}.part", uuid::Uuid::new_v4()));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            Ok(Box::new(LocalWriteSession {
                store,
                path,
                file: Some(file),
                kind,
                created_at,
                size: 0,
                hasher: blake3::Hasher::new(),
                committed: false,
            }) as Box<dyn BlobWriteSession>)
        })
    }

    fn open(
        &self,
        key: &BlobKey,
    ) -> PortFuture<'_, Result<Box<dyn BlobReadSession>, BlobStoreError>> {
        let path = self.final_path(key);
        let root = self.inner.objects.clone();
        Box::pin(async move {
            let path = path?;
            let canonical = fs::canonicalize(&path).await.map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    BlobStoreError::NotFound
                } else {
                    BlobStoreError::Unavailable
                }
            })?;
            if !canonical.starts_with(root) {
                return Err(BlobStoreError::Invalid);
            }
            let metadata = fs::symlink_metadata(&canonical)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(BlobStoreError::Invalid);
            }
            let file = File::open(canonical)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            Ok(Box::new(LocalReadSession { file }) as Box<dyn BlobReadSession>)
        })
    }

    fn delete(&self, key: &BlobKey) -> PortFuture<'_, Result<(), BlobStoreError>> {
        let path = self.final_path(key);
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let path = path?;
            let metadata = match fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(_) => return Err(BlobStoreError::Unavailable),
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(BlobStoreError::Invalid);
            }
            fs::remove_file(path)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            inner.used_bytes.fetch_sub(metadata.len(), Ordering::AcqRel);
            Ok(())
        })
    }

    fn scan(
        &self,
        request: BlobScanRequest,
    ) -> PortFuture<'_, Result<BlobScanPage, BlobStoreError>> {
        let root = self.inner.objects.clone();
        Box::pin(async move {
            if request.limit == 0 || request.limit > 10_000 {
                return Err(BlobStoreError::Invalid);
            }
            tokio::task::spawn_blocking(move || scan_objects(&root, request))
                .await
                .map_err(|_| BlobStoreError::Unavailable)?
        })
    }

    fn capacity(&self) -> BlobCapacity {
        BlobCapacity {
            used_bytes: self.inner.used_bytes.load(Ordering::Acquire),
            writable_bytes: self.inner.writable_bytes,
            reserve_bytes: self.inner.reserve_bytes,
        }
    }
}

struct LocalWriteSession {
    store: LocalBlobStore,
    path: PathBuf,
    file: Option<File>,
    kind: BlobKind,
    created_at: Timestamp,
    size: u64,
    hasher: blake3::Hasher,
    committed: bool,
}

impl LocalWriteSession {
    fn reserve(&self, bytes: u64) -> Result<(), BlobStoreError> {
        let inner = &self.store.inner;
        let mut current = inner.used_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return Err(BlobStoreError::Capacity);
            };
            if next > inner.writable_bytes {
                return Err(BlobStoreError::Capacity);
            }
            match inner.used_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }
}

impl BlobWriteSession for LocalWriteSession {
    fn write_chunk(&mut self, chunk: Box<[u8]>) -> PortFuture<'_, Result<(), BlobStoreError>> {
        Box::pin(async move {
            let length = u64::try_from(chunk.len()).map_err(|_| BlobStoreError::Capacity)?;
            let next = self
                .size
                .checked_add(length)
                .ok_or(BlobStoreError::Capacity)?;
            if next > self.store.inner.max_object_bytes {
                return Err(BlobStoreError::Capacity);
            }
            self.reserve(length)?;
            let Some(file) = self.file.as_mut() else {
                self.store
                    .inner
                    .used_bytes
                    .fetch_sub(length, Ordering::AcqRel);
                return Err(BlobStoreError::Invalid);
            };
            if file.write_all(&chunk).await.is_err() {
                self.store
                    .inner
                    .used_bytes
                    .fetch_sub(length, Ordering::AcqRel);
                return Err(BlobStoreError::Unavailable);
            }
            self.hasher.update(&chunk);
            self.size = next;
            Ok(())
        })
    }

    fn commit(
        mut self: Box<Self>,
        key: BlobKey,
    ) -> PortFuture<'static, Result<BlobObject, BlobStoreError>> {
        Box::pin(async move {
            let Some(mut file) = self.file.take() else {
                return Err(BlobStoreError::Invalid);
            };
            file.flush()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            file.sync_all()
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            drop(file);
            let checksum = BlobChecksum::from_bytes(*self.hasher.finalize().as_bytes());
            let final_path = self.store.final_path(&key)?;
            let parent = final_path.parent().ok_or(BlobStoreError::Invalid)?;
            fs::create_dir_all(parent)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            // A hard-link publishes the fully synced temporary inode without replacing an
            // already committed object. Temporary and final directories are deliberately
            // created below one root, so they are on the same filesystem.
            match fs::hard_link(&self.path, &final_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = verify_file(&final_path, self.size, checksum).await?;
                    if !existing {
                        return Err(BlobStoreError::Corrupt);
                    }
                    fs::remove_file(&self.path)
                        .await
                        .map_err(|_| BlobStoreError::Unavailable)?;
                    self.store
                        .inner
                        .used_bytes
                        .fetch_sub(self.size, Ordering::AcqRel);
                }
                Err(_) if fs::try_exists(&final_path).await.unwrap_or(false) => {
                    let existing = verify_file(&final_path, self.size, checksum).await?;
                    if !existing {
                        return Err(BlobStoreError::Corrupt);
                    }
                    fs::remove_file(&self.path)
                        .await
                        .map_err(|_| BlobStoreError::Unavailable)?;
                    self.store
                        .inner
                        .used_bytes
                        .fetch_sub(self.size, Ordering::AcqRel);
                }
                Err(_) => return Err(BlobStoreError::Unavailable),
            }
            if fs::try_exists(&self.path)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?
            {
                fs::remove_file(&self.path)
                    .await
                    .map_err(|_| BlobStoreError::Unavailable)?;
            }
            self.committed = true;
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
        Box::pin(async move {
            self.file.take();
            match fs::remove_file(&self.path).await {
                Ok(()) => {
                    self.store
                        .inner
                        .used_bytes
                        .fetch_sub(self.size, Ordering::AcqRel);
                    self.committed = true;
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(BlobStoreError::Unavailable),
            }
        })
    }
}

impl Drop for LocalWriteSession {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.file.take();
        if std::fs::remove_file(&self.path).is_ok() {
            self.store
                .inner
                .used_bytes
                .fetch_sub(self.size, Ordering::AcqRel);
        }
    }
}

struct LocalReadSession {
    file: File,
}

impl BlobReadSession for LocalReadSession {
    fn read_chunk(
        &mut self,
        maximum: usize,
    ) -> PortFuture<'_, Result<Option<Box<[u8]>>, BlobStoreError>> {
        Box::pin(async move {
            if maximum == 0 || maximum > 1024 * 1024 {
                return Err(BlobStoreError::Invalid);
            }
            let mut chunk = vec![0_u8; maximum];
            let count = self
                .file
                .read(&mut chunk)
                .await
                .map_err(|_| BlobStoreError::Unavailable)?;
            if count == 0 {
                return Ok(None);
            }
            chunk.truncate(count);
            Ok(Some(chunk.into_boxed_slice()))
        })
    }
}

async fn verify_file(
    path: &Path,
    expected_size: u64,
    expected_checksum: BlobChecksum,
) -> Result<bool, BlobStoreError> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|_| BlobStoreError::Unavailable)?;
    if metadata.len() != expected_size {
        return Ok(false);
    }
    let mut file = File::open(path)
        .await
        .map_err(|_| BlobStoreError::Unavailable)?;
    let mut hasher = blake3::Hasher::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut chunk)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    Ok(hasher.finalize().as_bytes() == &expected_checksum.as_bytes())
}

fn scan_objects(root: &Path, request: BlobScanRequest) -> Result<BlobScanPage, BlobStoreError> {
    let mut selected = BinaryHeap::with_capacity(request.limit.saturating_add(1));
    let namespace_root = root.join(request.namespace.prefix().trim_end_matches('/'));
    if namespace_root.exists() {
        select_page_keys(
            root,
            &namespace_root,
            request.cursor.as_deref(),
            request.older_than,
            request.limit.saturating_add(1),
            &mut selected,
        )?;
    }
    let mut keys = selected.into_vec();
    keys.sort_unstable();
    let has_more = keys.len() > request.limit;
    keys.truncate(request.limit);
    let last_examined = keys.last().cloned();
    let mut objects = Vec::with_capacity(keys.len());
    for key in keys {
        let full = root.join(key.replace('/', std::path::MAIN_SEPARATOR_STR));
        let metadata = std::fs::symlink_metadata(&full).map_err(|_| BlobStoreError::Unavailable)?;
        let key = BlobKey::new(key).map_err(|_| BlobStoreError::Invalid)?;
        let Ok(kind) = request.namespace.kind_for_key(&key) else {
            continue;
        };
        objects.push(BlobObject {
            kind,
            key,
            size: metadata.len(),
            checksum: checksum_file(&full)?,
            created_at: modified_timestamp(&metadata)?,
        });
    }
    let next_cursor = has_more
        .then(|| last_examined.map(String::into_boxed_str))
        .flatten();
    Ok(BlobScanPage {
        objects,
        next_cursor,
    })
}

fn select_page_keys(
    root: &Path,
    directory: &Path,
    cursor: Option<&str>,
    older_than: Timestamp,
    capacity: usize,
    selected: &mut BinaryHeap<String>,
) -> Result<(), BlobStoreError> {
    for entry in std::fs::read_dir(directory).map_err(|_| BlobStoreError::Unavailable)? {
        let entry = entry.map_err(|_| BlobStoreError::Unavailable)?;
        let file_type = entry.file_type().map_err(|_| BlobStoreError::Unavailable)?;
        if file_type.is_symlink() {
            return Err(BlobStoreError::Invalid);
        }
        let path = entry.path();
        if file_type.is_dir() {
            select_page_keys(root, &path, cursor, older_than, capacity, selected)?;
        } else if file_type.is_file() {
            let key = path
                .strip_prefix(root)
                .map_err(|_| BlobStoreError::Invalid)?
                .to_str()
                .ok_or(BlobStoreError::Invalid)?
                .replace('\\', "/");
            if cursor.is_some_and(|cursor| key.as_str() <= cursor) {
                continue;
            }
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|_| BlobStoreError::Unavailable)?;
            if modified_timestamp(&metadata)? > older_than {
                continue;
            }
            selected.push(key);
            if selected.len() > capacity {
                selected.pop();
            }
        }
    }
    Ok(())
}

fn modified_timestamp(metadata: &std::fs::Metadata) -> Result<Timestamp, BlobStoreError> {
    let modified_millis = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Timestamp::from_unix_millis(i64::try_from(modified_millis).unwrap_or(i64::MAX))
        .map_err(|_| BlobStoreError::Invalid)
}

fn checksum_file(path: &Path) -> Result<BlobChecksum, BlobStoreError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|_| BlobStoreError::Unavailable)?;
    let mut hasher = blake3::Hasher::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut chunk)
            .map_err(|_| BlobStoreError::Unavailable)?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    Ok(BlobChecksum::from_bytes(*hasher.finalize().as_bytes()))
}

fn directory_size(directory: &Path) -> Result<u64, BlobStoreError> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(directory).map_err(|_| BlobStoreError::Unavailable)? {
        let entry = entry.map_err(|_| BlobStoreError::Unavailable)?;
        let file_type = entry.file_type().map_err(|_| BlobStoreError::Unavailable)?;
        if file_type.is_symlink() {
            return Err(BlobStoreError::Invalid);
        }
        let bytes = if file_type.is_dir() {
            directory_size(&entry.path())?
        } else if file_type.is_file() {
            entry
                .metadata()
                .map_err(|_| BlobStoreError::Unavailable)?
                .len()
        } else {
            0
        };
        total = total.checked_add(bytes).ok_or(BlobStoreError::Capacity)?;
    }
    Ok(total)
}

async fn directory_bytes(path: PathBuf) -> Result<u64, BlobStoreError> {
    tokio::task::spawn_blocking(move || directory_size(&path))
        .await
        .map_err(|_| BlobStoreError::Unavailable)?
}

fn absolute(path: &Path) -> Result<PathBuf, BlobStoreError> {
    if path.as_os_str().is_empty() {
        return Err(BlobStoreError::Invalid);
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|_| BlobStoreError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faultkeep_domain::{EventId, ProjectId, blob::BlobObjectId};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("faultkeep-blob-{}", uuid::Uuid::new_v4())))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let root = std::env::temp_dir();
            let resolved = absolute(&self.0).expect("test path is valid");
            assert!(resolved.starts_with(root), "test cleanup must stay in temp");
            let _ = std::fs::remove_dir_all(&resolved);
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_millis(1_700_000_000_000).unwrap()
    }

    fn key(seed: u8) -> BlobKey {
        BlobKey::event_owned(
            ProjectId::new(7).unwrap(),
            EventId::from_bytes([seed; 16]),
            BlobObjectId::from_bytes([seed; 16]),
        )
    }

    async fn store(directory: &TestDirectory, writable: u64) -> LocalBlobStore {
        LocalBlobStore::new(
            &directory.0,
            LocalBlobConfig {
                capacity_bytes: writable + 128,
                reserve_bytes: 128,
                max_object_bytes: writable,
            },
        )
        .await
        .unwrap()
    }

    async fn publish(
        store: &LocalBlobStore,
        key: BlobKey,
        chunks: &[&[u8]],
    ) -> Result<BlobObject, BlobStoreError> {
        let mut session = store.begin(BlobKind::EventAttachment, now()).await?;
        for chunk in chunks {
            session.write_chunk((*chunk).into()).await?;
        }
        session.commit(key).await
    }

    #[tokio::test]
    async fn conformance_streams_and_reads_committed_object() {
        let directory = TestDirectory::new();
        let store = store(&directory, 1024).await;
        let object = publish(&store, key(1), &[b"hello ", b"world"])
            .await
            .unwrap();
        assert_eq!(object.size, 11);
        assert_eq!(
            object.checksum,
            BlobChecksum::from_bytes(*blake3::hash(b"hello world").as_bytes())
        );

        let mut reader = store.open(&object.key).await.unwrap();
        let mut bytes = Vec::new();
        while let Some(chunk) = reader.read_chunk(4).await.unwrap() {
            bytes.extend_from_slice(&chunk);
        }
        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn publication_is_idempotent_and_never_replaces_conflicting_bytes() {
        let directory = TestDirectory::new();
        let store = store(&directory, 1024).await;
        publish(&store, key(2), &[b"first"]).await.unwrap();
        publish(&store, key(2), &[b"first"]).await.unwrap();
        assert_eq!(
            publish(&store, key(2), &[b"other"]).await.unwrap_err(),
            BlobStoreError::Corrupt
        );
        let mut reader = store.open(&key(2)).await.unwrap();
        assert_eq!(
            reader.read_chunk(64).await.unwrap().unwrap().as_ref(),
            b"first"
        );
    }

    #[tokio::test]
    async fn uncommitted_and_aborted_objects_are_not_visible() {
        let directory = TestDirectory::new();
        let store = store(&directory, 1024).await;
        let mut dropped = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
        dropped
            .write_chunk(b"partial".as_slice().into())
            .await
            .unwrap();
        drop(dropped);
        assert_eq!(
            store.open(&key(3)).await.err(),
            Some(BlobStoreError::NotFound)
        );

        let mut aborted = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
        aborted
            .write_chunk(b"partial".as_slice().into())
            .await
            .unwrap();
        aborted.abort().await.unwrap();
        assert_eq!(store.capacity().used_bytes, 0);
    }

    #[tokio::test]
    async fn startup_removes_crash_left_temporary_objects() {
        let directory = TestDirectory::new();
        let temporary = directory.0.join("temporary");
        std::fs::create_dir_all(&temporary).unwrap();
        std::fs::write(temporary.join("crash.part"), b"partial").unwrap();
        let store = store(&directory, 1024).await;
        assert_eq!(store.capacity().used_bytes, 0);
        assert_eq!(std::fs::read_dir(temporary).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn reserve_and_object_limits_fail_before_disk_is_overcommitted() {
        let directory = TestDirectory::new();
        let store = store(&directory, 8).await;
        let mut session = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
        assert_eq!(
            session.write_chunk(vec![0_u8; 9].into_boxed_slice()).await,
            Err(BlobStoreError::Capacity)
        );
        assert_eq!(store.capacity().used_bytes, 0);
    }

    #[tokio::test]
    async fn concurrent_sessions_share_capacity_atomically() {
        let directory = TestDirectory::new();
        let store = store(&directory, 8).await;
        let first = publish(&store, key(4), &[b"123456"]);
        let second = publish(&store, key(5), &[b"abcdef"]);
        let (first, second) = tokio::join!(first, second);
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(store.capacity().used_bytes, 6);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "Phase 16 retained local filesystem performance baseline"]
    async fn performance_local_blob_rps_mib_concurrency_disk_full_and_slow_io() {
        const OBJECTS: u32 = 256;
        const OBJECT_BYTES: usize = 256 * 1024;
        const CONCURRENCY: u32 = 8;
        let directory = TestDirectory::new();
        let store = Arc::new(
            LocalBlobStore::new(
                &directory.0,
                LocalBlobConfig {
                    capacity_bytes: 80 * 1024 * 1024,
                    reserve_bytes: 8 * 1024 * 1024,
                    max_object_bytes: 1024 * 1024,
                },
            )
            .await
            .unwrap(),
        );
        let started = std::time::Instant::now();
        let mut tasks = Vec::new();
        for worker in 0..CONCURRENCY {
            let store = Arc::clone(&store);
            tasks.push(tokio::spawn(async move {
                let chunk = vec![worker as u8; 64 * 1024].into_boxed_slice();
                for index in (worker..OBJECTS).step_by(CONCURRENCY as usize) {
                    let event = EventId::from_bytes(u128::from(index + 1).to_be_bytes());
                    let object =
                        BlobObjectId::from_bytes(u128::from(OBJECTS - index).to_be_bytes());
                    let mut writer = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
                    for _ in 0..(OBJECT_BYTES / chunk.len()) {
                        writer.write_chunk(chunk.clone()).await.unwrap();
                    }
                    writer
                        .commit(BlobKey::event_owned(
                            ProjectId::new(7).unwrap(),
                            event,
                            object,
                        ))
                        .await
                        .unwrap();
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let elapsed = started.elapsed();
        let rps = f64::from(OBJECTS) / elapsed.as_secs_f64();
        let mib = f64::from(OBJECTS) * OBJECT_BYTES as f64 / (1024.0 * 1024.0);
        let mib_per_second = mib / elapsed.as_secs_f64();

        let mut oversized = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
        let object_limit_result = oversized
            .write_chunk(vec![0_u8; 9 * 1024 * 1024].into_boxed_slice())
            .await;
        assert_eq!(object_limit_result, Err(BlobStoreError::Capacity));

        let slow_started = std::time::Instant::now();
        let mut slow_tasks = Vec::new();
        for index in 0..16_u32 {
            let store = Arc::clone(&store);
            slow_tasks.push(tokio::spawn(async move {
                let mut writer = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
                for _ in 0..4 {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                    writer
                        .write_chunk(vec![index as u8; 1024].into_boxed_slice())
                        .await
                        .unwrap();
                }
                writer
                    .commit(BlobKey::event_owned(
                        ProjectId::new(8).unwrap(),
                        EventId::from_bytes(u128::from(index + 1).to_be_bytes()),
                        BlobObjectId::from_bytes(u128::from(index + 1).to_be_bytes()),
                    ))
                    .await
                    .unwrap();
            }));
        }
        for task in slow_tasks {
            task.await.unwrap();
        }
        let slow_rps = 16.0 / slow_started.elapsed().as_secs_f64();
        let mut filler_index = 0_u32;
        while store.capacity().used_bytes < store.capacity().writable_bytes {
            let remaining = store.capacity().writable_bytes - store.capacity().used_bytes;
            let bytes = remaining.min(1024 * 1024);
            let mut writer = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
            writer
                .write_chunk(vec![0_u8; bytes as usize].into_boxed_slice())
                .await
                .unwrap();
            filler_index += 1;
            writer
                .commit(BlobKey::event_owned(
                    ProjectId::new(9).unwrap(),
                    EventId::from_bytes(u128::from(filler_index).to_be_bytes()),
                    BlobObjectId::from_bytes(u128::from(filler_index).to_be_bytes()),
                ))
                .await
                .unwrap();
        }
        let mut disk_full = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
        assert_eq!(
            disk_full.write_chunk(Box::from([1_u8])).await,
            Err(BlobStoreError::Capacity)
        );
        eprintln!(
            "{{\"objects\":{OBJECTS},\"object_bytes\":{OBJECT_BYTES},\"concurrency\":{CONCURRENCY},\"rps\":{rps:.2},\"mib_per_second\":{mib_per_second:.2},\"slow_io_rps\":{slow_rps:.2},\"elapsed_ms\":{}}}",
            elapsed.as_millis()
        );
        assert!(rps > 1.0);
        assert!(mib_per_second > 1.0);
        assert!(slow_rps > 1.0);
    }
}
