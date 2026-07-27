use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Request, Response, StatusCode},
    routing::any,
};
use metric_blob::{LocalBlobConfig, LocalBlobStore, S3BlobConfig, S3BlobStore};
use metric_domain::{
    EventId, ProjectId, Timestamp,
    archive::ArchiveSegmentId,
    blob::{BlobKey, BlobKind, BlobObjectId},
};
use metric_ports::{BlobScanRequest, BlobStore, BlobStoreError};
use tokio::{net::TcpListener, task::JoinHandle};

#[derive(Default)]
struct EmulatorState {
    objects: Mutex<HashMap<String, StoredObject>>,
    uploads: Mutex<HashMap<String, MultipartUpload>>,
    fail_upload_parts: AtomicUsize,
    deny: AtomicBool,
}

#[derive(Clone)]
struct StoredObject {
    bytes: Vec<u8>,
    metadata: HashMap<String, String>,
}

struct MultipartUpload {
    key: String,
    parts: BTreeMap<u32, Vec<u8>>,
}

struct Emulator {
    endpoint: String,
    state: Arc<EmulatorState>,
    task: JoinHandle<()>,
}

impl Emulator {
    async fn start() -> Self {
        let state = Arc::new(EmulatorState::default());
        let router = Router::new()
            .route("/{bucket}", any(handle_bucket))
            .route("/{bucket}/", any(handle_bucket))
            .route("/{bucket}/{*key}", any(handle_object))
            .with_state(Arc::clone(&state));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_bucket(
    State(state): State<Arc<EmulatorState>>,
    request: Request<Body>,
) -> Response<Body> {
    if state.deny.load(Ordering::Acquire) {
        return response(StatusCode::FORBIDDEN, error_xml("AccessDenied"));
    }
    if request.method() != Method::GET || !query(&request).contains_key("list-type") {
        return response(StatusCode::NOT_FOUND, error_xml("NoSuchBucket"));
    }
    let values = query(&request);
    let prefix = values.get("prefix").map_or("", String::as_str);
    let start_after = values.get("start-after").map_or("", String::as_str);
    let maximum = values
        .get("max-keys")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1_000);
    let objects = lock(&state.objects);
    let mut keys = objects
        .keys()
        .filter(|key| key.starts_with(prefix) && key.as_str() > start_after)
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    let truncated = keys.len() > maximum;
    keys.truncate(maximum);
    let mut xml = format!(
        "<ListBucketResult><Name>metric-test</Name><Prefix>{prefix}</Prefix><IsTruncated>{truncated}</IsTruncated>"
    );
    for key in keys {
        let size = objects.get(&key).map_or(0, |object| object.bytes.len());
        xml.push_str(&format!(
            "<Contents><Key>{key}</Key><LastModified>2026-07-24T00:00:00Z</LastModified><ETag>\"etag\"</ETag><Size>{size}</Size><StorageClass>STANDARD</StorageClass></Contents>"
        ));
    }
    xml.push_str("</ListBucketResult>");
    response(StatusCode::OK, xml)
}

async fn handle_object(
    State(state): State<Arc<EmulatorState>>,
    request: Request<Body>,
) -> Response<Body> {
    if state.deny.load(Ordering::Acquire) {
        return response(StatusCode::FORBIDDEN, error_xml("AccessDenied"));
    }
    let key = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split_once('/')
        .map(|(_, key)| key.to_owned())
        .unwrap_or_default();
    let values = query(&request);
    match *request.method() {
        Method::POST if values.contains_key("uploads") => {
            let upload_id = format!("upload-{}", uuid::Uuid::new_v4());
            lock(&state.uploads).insert(
                upload_id.clone(),
                MultipartUpload {
                    key: key.clone(),
                    parts: BTreeMap::new(),
                },
            );
            response(
                StatusCode::OK,
                format!(
                    "<InitiateMultipartUploadResult><Bucket>metric-test</Bucket><Key>{key}</Key><UploadId>{upload_id}</UploadId></InitiateMultipartUploadResult>"
                ),
            )
        }
        Method::PUT if values.contains_key("partNumber") => {
            if state
                .fail_upload_parts
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                return response(StatusCode::SERVICE_UNAVAILABLE, error_xml("SlowDown"));
            }
            let upload_id = values.get("uploadId").cloned().unwrap_or_default();
            let part = values
                .get("partNumber")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or_default();
            let bytes = to_bytes(request.into_body(), 70 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec();
            let mut uploads = lock(&state.uploads);
            let Some(upload) = uploads.get_mut(&upload_id) else {
                return response(StatusCode::NOT_FOUND, error_xml("NoSuchUpload"));
            };
            upload.parts.insert(part, bytes);
            Response::builder()
                .status(StatusCode::OK)
                .header("etag", format!("\"part-{part}\""))
                .body(Body::empty())
                .unwrap()
        }
        Method::POST if values.contains_key("uploadId") => {
            let upload_id = values.get("uploadId").cloned().unwrap_or_default();
            let Some(upload) = lock(&state.uploads).remove(&upload_id) else {
                return response(StatusCode::NOT_FOUND, error_xml("NoSuchUpload"));
            };
            let mut bytes = Vec::new();
            for part in upload.parts.into_values() {
                bytes.extend_from_slice(&part);
            }
            lock(&state.objects).insert(
                upload.key.clone(),
                StoredObject {
                    bytes,
                    metadata: HashMap::new(),
                },
            );
            response(
                StatusCode::OK,
                format!(
                    "<CompleteMultipartUploadResult><Location>local</Location><Bucket>metric-test</Bucket><Key>{}</Key><ETag>\"complete\"</ETag></CompleteMultipartUploadResult>",
                    upload.key
                ),
            )
        }
        Method::DELETE if values.contains_key("uploadId") => {
            let upload_id = values.get("uploadId").cloned().unwrap_or_default();
            lock(&state.uploads).remove(&upload_id);
            response(StatusCode::NO_CONTENT, "")
        }
        Method::PUT => {
            let metadata = metadata(request.headers());
            if let Some(source) = request.headers().get("x-amz-copy-source") {
                let source = source
                    .to_str()
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .split_once('/')
                    .map(|(_, key)| decode_path(key))
                    .unwrap_or_default();
                let Some(source) = lock(&state.objects).get(&source).cloned() else {
                    return response(StatusCode::NOT_FOUND, error_xml("NoSuchKey"));
                };
                lock(&state.objects).insert(
                    key.clone(),
                    StoredObject {
                        bytes: source.bytes,
                        metadata,
                    },
                );
                return response(
                    StatusCode::OK,
                    "<CopyObjectResult><ETag>\"copy\"</ETag><LastModified>2026-07-24T00:00:00Z</LastModified></CopyObjectResult>",
                );
            }
            let bytes = to_bytes(request.into_body(), 70 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec();
            lock(&state.objects).insert(key, StoredObject { bytes, metadata });
            response(StatusCode::OK, "")
        }
        Method::HEAD => {
            let objects = lock(&state.objects);
            let Some(object) = objects.get(&key) else {
                return response(StatusCode::NOT_FOUND, "");
            };
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("content-length", object.bytes.len().to_string())
                .header("etag", "\"etag\"");
            for (name, value) in &object.metadata {
                builder = builder.header(format!("x-amz-meta-{name}"), value);
            }
            builder.body(Body::empty()).unwrap()
        }
        Method::GET => {
            let Some(object) = lock(&state.objects).get(&key).cloned() else {
                return response(StatusCode::NOT_FOUND, error_xml("NoSuchKey"));
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("content-length", object.bytes.len().to_string())
                .body(Body::from(object.bytes))
                .unwrap()
        }
        Method::DELETE => {
            lock(&state.objects).remove(&key);
            response(StatusCode::NO_CONTENT, "")
        }
        _ => response(StatusCode::BAD_REQUEST, error_xml("InvalidRequest")),
    }
}

fn query(request: &Request<Body>) -> HashMap<String, String> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

fn metadata(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            name.as_str()
                .strip_prefix("x-amz-meta-")
                .zip(value.to_str().ok())
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn decode_path(value: &str) -> String {
    value
        .replace("%2F", "/")
        .replace("%2f", "/")
        .replace("%20", " ")
}

fn response(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(body.into())
        .unwrap()
}

fn error_xml(code: &str) -> String {
    format!("<Error><Code>{code}</Code><Message>emulated failure</Message></Error>")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now() -> Timestamp {
    Timestamp::from_unix_millis(1_700_000_000_000).unwrap()
}

fn attachment_key(seed: u8) -> BlobKey {
    BlobKey::event_owned(
        ProjectId::new(7).unwrap(),
        EventId::from_bytes([seed; 16]),
        BlobObjectId::from_bytes([seed; 16]),
    )
}

async fn shared_conformance(store: Arc<dyn BlobStore>) {
    let key = attachment_key(1);
    let mut writer = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
    writer
        .write_chunk(b"hello ".as_slice().into())
        .await
        .unwrap();
    assert_eq!(store.open(&key).await.err(), Some(BlobStoreError::NotFound));
    writer
        .write_chunk(b"world".as_slice().into())
        .await
        .unwrap();
    let object = writer.commit(key.clone()).await.unwrap();
    assert_eq!(object.size, 11);

    let mut reader = store.open(&key).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.read_chunk(4).await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    assert_eq!(bytes, b"hello world");

    for payload in [b"hello world".as_slice(), b"conflict".as_slice()] {
        let mut retry = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
        retry.write_chunk(payload.into()).await.unwrap();
        let result = retry.commit(key.clone()).await;
        if payload == b"hello world" {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err(), BlobStoreError::Corrupt);
        }
    }
    let archive_key = BlobKey::event_archive(
        ProjectId::new(7).unwrap(),
        2026,
        7,
        24,
        ArchiveSegmentId::from_bytes([9; 16]),
    );
    let mut archive = store.begin(BlobKind::EventArchive, now()).await.unwrap();
    archive
        .write_chunk(b"PAR1".as_slice().into())
        .await
        .unwrap();
    archive.commit(archive_key).await.unwrap();
    let page = store
        .scan(BlobScanRequest {
            namespace: metric_domain::blob::BlobNamespace::EventOwned,
            older_than: Timestamp::from_unix_millis(2_000_000_000_000).unwrap(),
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].key, key);
    store.delete(&key).await.unwrap();
    assert_eq!(store.open(&key).await.err(), Some(BlobStoreError::NotFound));
}

#[tokio::test]
async fn local_and_s3_emulator_share_blobstore_conformance() {
    let root =
        std::env::temp_dir().join(format!("metric-blob-conformance-{}", uuid::Uuid::new_v4()));
    let local: Arc<dyn BlobStore> = Arc::new(
        LocalBlobStore::new(
            &root,
            LocalBlobConfig {
                capacity_bytes: 32 * 1024 * 1024,
                reserve_bytes: 1024,
                max_object_bytes: 16 * 1024 * 1024,
            },
        )
        .await
        .unwrap(),
    );
    shared_conformance(local).await;
    std::fs::remove_dir_all(&root).unwrap();

    let emulator = Emulator::start().await;
    let s3: Arc<dyn BlobStore> = Arc::new(
        S3BlobStore::new(S3BlobConfig {
            endpoint: Some(emulator.endpoint.clone().into()),
            region: "us-east-1".into(),
            bucket: "metric-test".into(),
            access_key_id: "test-access".into(),
            secret_access_key: "test-secret".into(),
            session_token: None,
            force_path_style: true,
            part_bytes: 5 * 1024 * 1024,
            max_object_bytes: 16 * 1024 * 1024,
        })
        .unwrap(),
    );
    shared_conformance(s3).await;
}

#[tokio::test]
async fn s3_emulator_retries_multipart_and_maps_missing_and_permission_failures() {
    let emulator = Emulator::start().await;
    emulator.state.fail_upload_parts.store(1, Ordering::Release);
    let store = S3BlobStore::new(S3BlobConfig {
        endpoint: Some(emulator.endpoint.clone().into()),
        region: "us-east-1".into(),
        bucket: "metric-test".into(),
        access_key_id: "test-access".into(),
        secret_access_key: "test-secret".into(),
        session_token: None,
        force_path_style: true,
        part_bytes: 5 * 1024 * 1024,
        max_object_bytes: 16 * 1024 * 1024,
    })
    .unwrap();
    let key = attachment_key(4);
    let mut writer = store.begin(BlobKind::EventAttachment, now()).await.unwrap();
    writer
        .write_chunk(vec![7_u8; 6 * 1024 * 1024].into_boxed_slice())
        .await
        .unwrap();
    let object = writer.commit(key.clone()).await.unwrap();
    assert_eq!(object.size, 6 * 1024 * 1024);
    assert_eq!(
        store.open(&attachment_key(5)).await.err(),
        Some(BlobStoreError::NotFound)
    );
    emulator.state.deny.store(true, Ordering::Release);
    assert_eq!(
        store.open(&key).await.err(),
        Some(BlobStoreError::Unavailable)
    );
}

#[tokio::test]
#[ignore = "selected real-compatible matrix; requires METRIC_S3_TEST_* credentials"]
async fn selected_real_compatible_service_matrix() {
    let endpoint = std::env::var("METRIC_S3_TEST_ENDPOINT").ok();
    let region = std::env::var("METRIC_S3_TEST_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
    let bucket = std::env::var("METRIC_S3_TEST_BUCKET")
        .expect("METRIC_S3_TEST_BUCKET is required for the selected matrix");
    let access_key_id = std::env::var("METRIC_S3_TEST_ACCESS_KEY_ID")
        .expect("METRIC_S3_TEST_ACCESS_KEY_ID is required for the selected matrix");
    let secret_access_key = std::env::var("METRIC_S3_TEST_SECRET_ACCESS_KEY")
        .expect("METRIC_S3_TEST_SECRET_ACCESS_KEY is required for the selected matrix");
    let store: Arc<dyn BlobStore> = Arc::new(
        S3BlobStore::new(S3BlobConfig {
            endpoint: endpoint.map(Into::into),
            region: region.into(),
            bucket: bucket.into(),
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token: std::env::var("METRIC_S3_TEST_SESSION_TOKEN")
                .ok()
                .map(Into::into),
            force_path_style: true,
            part_bytes: 5 * 1024 * 1024,
            max_object_bytes: 16 * 1024 * 1024,
        })
        .unwrap(),
    );
    shared_conformance(store).await;
}
