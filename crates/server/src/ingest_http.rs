use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_compression::tokio::bufread::{GzipDecoder, ZlibDecoder};
use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, OriginalUri, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use metric_application::{
    ingest::{
        DisabledCategory, DiscardedItem, IngestError, IngestErrorKind, IngestRequest, IngestResult,
        IngestService, MinidumpRequest, PendingAttachment, PendingSignal, PendingSignalKind,
        PrimaryEvent,
    },
    observability::{Metric, Metrics, Outcome, RequestId},
    shutdown::ShutdownSignal,
};
use metric_domain::{DsnKey, EventId, ProjectId};
use metric_ports::{
    BlobChunkSource, BlobStoreError, IngestOutcome, IngestOutcomeKind, PortFuture,
};
use metric_sentry_protocol::{
    AttachmentLimits, EnvelopeLimits, ParsedEnvelope, ProtocolError, ProtocolErrorKind,
    RawSignalKind, parse_envelope_with_attachments, parse_query_auth, parse_store_event,
    parse_x_sentry_auth,
};
use futures_util::{TryStreamExt, future};
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, BufReader},
    sync::Semaphore,
    time::timeout,
};
use tokio_util::io::StreamReader;

use crate::config::IngestConfig;

#[derive(Clone)]
struct IngestHttpState {
    service: Arc<IngestService>,
    config: IngestConfig,
    active: Arc<Semaphore>,
    parsing: Arc<Semaphore>,
    shutdown: ShutdownSignal,
}

#[derive(Serialize)]
struct SuccessResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
}

pub fn router(
    service: Arc<IngestService>,
    config: IngestConfig,
    shutdown: ShutdownSignal,
) -> Router {
    let parsing_tasks = if config.max_parsing_tasks == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        config.max_parsing_tasks
    };
    let state = IngestHttpState {
        service,
        active: Arc::new(Semaphore::new(config.max_active_requests)),
        parsing: Arc::new(Semaphore::new(parsing_tasks)),
        config,
        shutdown,
    };
    Router::new()
        .route("/api/{project_id}/envelope/", post(envelope_handler))
        .route("/api/{project_id}/store/", post(store_handler))
        .route("/api/{project_id}/minidump/", post(minidump_handler))
        .with_state(state)
}

async fn minidump_handler(
    State(state): State<IngestHttpState>,
    Path(project_id): Path<i32>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let result = timeout(
        state.config.request_timeout.get(),
        process_minidump(&state, project_id, uri.query(), &headers, body),
    )
    .await;
    match result {
        Ok(Ok(result)) => {
            minidump_success_response(result, state.config.unsupported_backoff_seconds)
        }
        Ok(Err(error)) => {
            state.service.record_outcome(error.outcome());
            map_http_error(request_id, error)
        }
        Err(_) => error_response(
            request_id,
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "request deadline exceeded",
            Some(1),
        ),
    }
}

async fn process_minidump(
    state: &IngestHttpState,
    project_id: i32,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Body,
) -> Result<IngestResult, HttpIngestError> {
    let _active = state
        .active
        .clone()
        .try_acquire_owned()
        .map_err(|_| HttpIngestError::RateLimited)?;
    let path_project_id =
        ProjectId::new(project_id).map_err(|_| HttpIngestError::Protocol("invalid_project_id"))?;
    let mut auth_keys = Vec::with_capacity(2);
    if let Some(value) = headers.get("x-sentry-auth") {
        auth_keys.push(parse_x_sentry_auth(
            value
                .to_str()
                .map_err(|_| HttpIngestError::Protocol("invalid_auth_header"))?,
        )?);
    }
    if let Some(key) = parse_query_auth(query.unwrap_or_default())? {
        auth_keys.push(key);
    }
    let supplied_event_id = parse_minidump_event_id(query, headers)?;
    let source = decoded_body_source(body, headers, &state.config)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let source: Box<dyn BlobChunkSource> = if content_type.split(';').next().is_some_and(|value| {
        value
            .trim()
            .eq_ignore_ascii_case("application/octet-stream")
    }) {
        source
    } else if content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"))
    {
        let boundary = multipart_boundary(content_type)?;
        Box::new(MultipartMinidumpSource::new(source, boundary))
    } else {
        return Err(HttpIngestError::Protocol("invalid_minidump_content_type"));
    };
    state
        .service
        .ingest_minidump(
            MinidumpRequest {
                path_project_id,
                auth_keys,
                dsn_project_id: None,
                supplied_event_id,
            },
            source,
        )
        .await
        .map_err(Into::into)
}

fn parse_minidump_event_id(
    query: Option<&str>,
    headers: &HeaderMap,
) -> Result<Option<EventId>, HttpIngestError> {
    let header_id = headers
        .get("sentry-event-id")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| HttpIngestError::Protocol("invalid_event_id"))
                .and_then(|value| {
                    EventId::parse(value).map_err(|_| HttpIngestError::Protocol("invalid_event_id"))
                })
        })
        .transpose()?;
    let mut query_id = None;
    for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        if matches!(name.as_ref(), "sentry_event_id" | "sentry[event_id]") {
            let parsed = EventId::parse(&value)
                .map_err(|_| HttpIngestError::Protocol("invalid_event_id"))?;
            if query_id.replace(parsed).is_some() {
                return Err(HttpIngestError::Protocol("conflicting_event_id"));
            }
        }
    }
    if header_id.is_some() && query_id.is_some() && header_id != query_id {
        return Err(HttpIngestError::Protocol("conflicting_event_id"));
    }
    Ok(header_id.or(query_id))
}

async fn envelope_handler(
    State(state): State<IngestHttpState>,
    Path(project_id): Path<i32>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    execute_request(
        state,
        project_id,
        request_id,
        uri.query(),
        headers,
        body,
        true,
    )
    .await
}

async fn store_handler(
    State(state): State<IngestHttpState>,
    Path(project_id): Path<i32>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    execute_request(
        state,
        project_id,
        request_id,
        uri.query(),
        headers,
        body,
        false,
    )
    .await
}

async fn execute_request(
    state: IngestHttpState,
    project_id: i32,
    request_id: RequestId,
    query: Option<&str>,
    headers: HeaderMap,
    body: Body,
    is_envelope: bool,
) -> Response {
    if state.shutdown.is_cancelled() {
        state.service.record_outcome(IngestOutcome {
            kind: IngestOutcomeKind::StorageUnavailable,
            reason: "shutting_down",
            quantity: 1,
        });
        return error_response(
            request_id,
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "temporarily unavailable",
            Some(1),
        );
    }
    let Ok(_active) = state.active.clone().try_acquire_owned() else {
        state.service.record_outcome(IngestOutcome {
            kind: IngestOutcomeKind::RateLimited,
            reason: "request_capacity",
            quantity: 1,
        });
        return error_response(
            request_id,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "request capacity exceeded",
            Some(1),
        );
    };
    let deadline = state.config.request_timeout.get();
    let response = match timeout(
        deadline,
        process_request(&state, project_id, query, &headers, body, is_envelope),
    )
    .await
    {
        Ok(Ok(result)) => success_response(result, state.config.unsupported_backoff_seconds),
        Ok(Err(error)) => {
            state.service.record_outcome(error.outcome());
            map_http_error(request_id, error)
        }
        Err(_) => {
            state.service.record_outcome(IngestOutcome {
                kind: IngestOutcomeKind::StorageUnavailable,
                reason: "timeout",
                quantity: 1,
            });
            error_response(
                request_id,
                StatusCode::SERVICE_UNAVAILABLE,
                "timeout",
                "request deadline exceeded",
                Some(1),
            )
        }
    };
    Metrics.increment(
        Metric::IngestRequests,
        if response.status().is_success() {
            Outcome::Ok
        } else if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            Outcome::Error
        } else {
            Outcome::Rejected
        },
    );
    response
}

async fn process_request(
    state: &IngestHttpState,
    project_id: i32,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Body,
    is_envelope: bool,
) -> Result<IngestResult, HttpIngestError> {
    let path_project_id =
        ProjectId::new(project_id).map_err(|_| HttpIngestError::Protocol("invalid_project_id"))?;
    let decoded = decode_body(body, headers, &state.config).await?;
    let parsing = state
        .parsing
        .clone()
        .try_acquire_owned()
        .map_err(|_| HttpIngestError::RateLimited)?;
    let mut auth_keys = Vec::with_capacity(3);
    if let Some(value) = headers.get("x-sentry-auth") {
        let value = value
            .to_str()
            .map_err(|_| HttpIngestError::Protocol("invalid_auth_header"))?;
        auth_keys.push(parse_x_sentry_auth(value)?);
    }
    if let Some(key) = parse_query_auth(query.unwrap_or_default())? {
        auth_keys.push(key);
    }
    let parsed = if is_envelope {
        parse_envelope_with_attachments(
            &decoded,
            EnvelopeLimits {
                max_items: state.config.max_envelope_items,
                max_event_bytes: state.config.max_event_bytes,
            },
            AttachmentLimits {
                max_count: if state.config.attachments.enabled {
                    state.config.attachments.max_count
                } else {
                    0
                },
                max_item_bytes: state.config.attachments.max_item_bytes,
                max_total_bytes: state.config.attachments.max_total_bytes,
            },
        )?
    } else {
        ParsedEnvelope {
            event_id: None,
            dsn: None,
            primary: Some(parse_store_event(&decoded, state.config.max_event_bytes)?),
            signals: Vec::new(),
            attachments: Vec::new(),
            discarded: Vec::new(),
            client_report_quantity: 0,
        }
    };
    if let Some(dsn) = &parsed.dsn {
        auth_keys.push(dsn.key);
    }
    let request = map_request(path_project_id, auth_keys, parsed);
    drop(parsing);
    state.service.ingest(request).await.map_err(Into::into)
}

fn map_request(
    path_project_id: ProjectId,
    auth_keys: Vec<DsnKey>,
    parsed: ParsedEnvelope,
) -> IngestRequest {
    IngestRequest {
        path_project_id,
        auth_keys,
        dsn_project_id: parsed.dsn.map(|dsn| dsn.project_id),
        envelope_event_id: parsed.event_id,
        primary: parsed.primary.map(|event| PrimaryEvent {
            header_event_id: event.header_event_id,
            raw_json: event.bytes,
        }),
        signals: parsed
            .signals
            .into_iter()
            .map(|signal| PendingSignal {
                kind: match signal.kind {
                    RawSignalKind::Log => PendingSignalKind::Log,
                    RawSignalKind::Transaction => PendingSignalKind::Transaction,
                    RawSignalKind::Span => PendingSignalKind::Span,
                },
                raw_json: signal.bytes,
            })
            .collect(),
        attachments: parsed
            .attachments
            .into_iter()
            .map(|attachment| PendingAttachment {
                position: attachment.position,
                filename: attachment.filename,
                content_type: attachment.content_type,
                attachment_type: attachment.attachment_type,
                bytes: attachment.bytes,
            })
            .collect(),
        discarded: parsed
            .discarded
            .into_iter()
            .map(|item| DiscardedItem {
                category: item.category.map(map_category),
                reason: item.reason,
            })
            .collect(),
        client_report_quantity: parsed.client_report_quantity,
    }
}

const fn map_category(category: metric_sentry_protocol::DisabledCategory) -> DisabledCategory {
    use metric_sentry_protocol::DisabledCategory as Wire;
    match category {
        Wire::Transaction => DisabledCategory::Transaction,
        Wire::Session => DisabledCategory::Session,
        Wire::Profile => DisabledCategory::Profile,
        Wire::Replay => DisabledCategory::Replay,
        Wire::CheckIn => DisabledCategory::CheckIn,
        Wire::Span => DisabledCategory::Span,
        Wire::Statsd => DisabledCategory::Statsd,
        Wire::Attachment => DisabledCategory::Attachment,
        Wire::OtherKnown => DisabledCategory::OtherKnown,
    }
}

struct HttpBodySource {
    reader: Pin<Box<dyn AsyncRead + Send>>,
    compressed_exceeded: Arc<AtomicBool>,
    decompressed_bytes: usize,
    max_decompressed_bytes: usize,
}

impl BlobChunkSource for HttpBodySource {
    fn next_chunk(
        &mut self,
        maximum: usize,
    ) -> PortFuture<'_, Result<Option<Box<[u8]>>, BlobStoreError>> {
        Box::pin(async move {
            if maximum == 0 || maximum > 1024 * 1024 {
                return Err(BlobStoreError::Invalid);
            }
            let mut chunk = vec![0_u8; maximum];
            let count = self.reader.as_mut().read(&mut chunk).await.map_err(|_| {
                if self.compressed_exceeded.load(Ordering::Relaxed) {
                    BlobStoreError::TooLarge
                } else {
                    BlobStoreError::Invalid
                }
            })?;
            if count == 0 {
                return Ok(None);
            }
            self.decompressed_bytes = self
                .decompressed_bytes
                .checked_add(count)
                .ok_or(BlobStoreError::TooLarge)?;
            if self.decompressed_bytes > self.max_decompressed_bytes {
                return Err(BlobStoreError::TooLarge);
            }
            chunk.truncate(count);
            Ok(Some(chunk.into_boxed_slice()))
        })
    }
}

fn decoded_body_source(
    body: Body,
    headers: &HeaderMap,
    config: &IngestConfig,
) -> Result<Box<dyn BlobChunkSource>, HttpIngestError> {
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > config.max_compressed_request_bytes)
    {
        return Err(HttpIngestError::TooLarge("compressed_request_too_large"));
    }
    let seen = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let seen_stream = Arc::clone(&seen);
    let exceeded_stream = Arc::clone(&exceeded);
    let max_compressed = config.max_compressed_request_bytes;
    let stream = body
        .into_data_stream()
        .map_err(io::Error::other)
        .and_then(move |chunk| {
            let total = seen_stream.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();
            let result = if total > max_compressed {
                exceeded_stream.store(true, Ordering::Relaxed);
                Err(io::Error::other("compressed limit exceeded"))
            } else {
                Ok(chunk)
            };
            future::ready(result)
        });
    let reader = BufReader::new(StreamReader::new(stream));
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .map(|value| value.to_str().unwrap_or("invalid"))
        .unwrap_or("identity");
    let reader: Pin<Box<dyn AsyncRead + Send>> = match encoding {
        "" | "identity" => Box::pin(reader),
        "gzip" => Box::pin(GzipDecoder::new(reader)),
        "deflate" => Box::pin(ZlibDecoder::new(reader)),
        _ => return Err(HttpIngestError::UnsupportedEncoding),
    };
    Ok(Box::new(HttpBodySource {
        reader,
        compressed_exceeded: exceeded,
        decompressed_bytes: 0,
        max_decompressed_bytes: config.max_decompressed_request_bytes,
    }))
}

struct MultipartMinidumpSource {
    inner: Box<dyn BlobChunkSource>,
    boundary: Box<[u8]>,
    buffer: Vec<u8>,
    initialized: bool,
    ended: bool,
}

impl MultipartMinidumpSource {
    fn new(inner: Box<dyn BlobChunkSource>, boundary: Box<str>) -> Self {
        Self {
            inner,
            boundary: format!("\r\n--{boundary}").into_bytes().into_boxed_slice(),
            buffer: Vec::new(),
            initialized: false,
            ended: false,
        }
    }
}

impl BlobChunkSource for MultipartMinidumpSource {
    fn next_chunk(
        &mut self,
        maximum: usize,
    ) -> PortFuture<'_, Result<Option<Box<[u8]>>, BlobStoreError>> {
        Box::pin(async move {
            if maximum == 0 || maximum > 1024 * 1024 {
                return Err(BlobStoreError::Invalid);
            }
            if self.ended {
                return Ok(None);
            }
            if !self.initialized {
                loop {
                    if let Some(end) = find_bytes(&self.buffer, b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&self.buffer[..end])
                            .map_err(|_| BlobStoreError::Invalid)?;
                        let lowercase = headers.to_ascii_lowercase();
                        if !self.buffer[..end].starts_with(&self.boundary[2..])
                            || !lowercase.contains("content-disposition:")
                            || !lowercase.contains("name=\"upload_file_minidump\"")
                        {
                            return Err(BlobStoreError::Invalid);
                        }
                        self.buffer.drain(..end + 4);
                        self.initialized = true;
                        break;
                    }
                    if self.buffer.len() > 16 * 1024 {
                        return Err(BlobStoreError::TooLarge);
                    }
                    let Some(chunk) = self.inner.next_chunk(16 * 1024).await? else {
                        return Err(BlobStoreError::Invalid);
                    };
                    self.buffer.extend_from_slice(&chunk);
                }
            }
            loop {
                if let Some(boundary) = find_bytes(&self.buffer, &self.boundary) {
                    if boundary == 0 {
                        self.ended = true;
                        return Ok(None);
                    }
                    let count = boundary.min(maximum);
                    return Ok(Some(
                        self.buffer
                            .drain(..count)
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    ));
                }
                let preserved = self.boundary.len().saturating_sub(1);
                if self.buffer.len() > preserved {
                    let count = (self.buffer.len() - preserved).min(maximum);
                    return Ok(Some(
                        self.buffer
                            .drain(..count)
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                    ));
                }
                let Some(chunk) = self.inner.next_chunk(maximum.max(4096)).await? else {
                    return Err(BlobStoreError::Invalid);
                };
                self.buffer.extend_from_slice(&chunk);
            }
        })
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty() && haystack.len() >= needle.len())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn multipart_boundary(content_type: &str) -> Result<Box<str>, HttpIngestError> {
    let boundary = content_type
        .split(';')
        .skip(1)
        .find_map(|parameter| {
            parameter
                .trim()
                .strip_prefix("boundary=")
                .map(|value| value.trim_matches('"'))
        })
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&byte))
        })
        .ok_or(HttpIngestError::Protocol("invalid_multipart_boundary"))?;
    Ok(boundary.into())
}

async fn decode_body(
    body: Body,
    headers: &HeaderMap,
    config: &IngestConfig,
) -> Result<Vec<u8>, HttpIngestError> {
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > config.max_compressed_request_bytes)
    {
        return Err(HttpIngestError::TooLarge("compressed_request_too_large"));
    }
    let seen = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    let seen_stream = Arc::clone(&seen);
    let exceeded_stream = Arc::clone(&exceeded);
    let max_compressed = config.max_compressed_request_bytes;
    let stream = body
        .into_data_stream()
        .map_err(io::Error::other)
        .and_then(move |chunk| {
            let total = seen_stream.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();
            let result = if total > max_compressed {
                exceeded_stream.store(true, Ordering::Relaxed);
                Err(io::Error::other("compressed limit exceeded"))
            } else {
                Ok(chunk)
            };
            future::ready(result)
        });
    let reader = BufReader::new(StreamReader::new(stream));
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .map(|value| value.to_str().unwrap_or("invalid"))
        .unwrap_or("identity");
    let result = match encoding {
        "" | "identity" => read_bounded(reader, config.max_decompressed_request_bytes).await,
        "gzip" => {
            read_bounded(
                GzipDecoder::new(reader),
                config.max_decompressed_request_bytes,
            )
            .await
        }
        "deflate" => {
            read_bounded(
                ZlibDecoder::new(reader),
                config.max_decompressed_request_bytes,
            )
            .await
        }
        _ => return Err(HttpIngestError::UnsupportedEncoding),
    };
    match result {
        Ok(bytes) => Ok(bytes),
        Err(ReadError::TooLarge) => {
            Err(HttpIngestError::TooLarge("decompressed_request_too_large"))
        }
        Err(ReadError::Io) if exceeded.load(Ordering::Relaxed) => {
            Err(HttpIngestError::TooLarge("compressed_request_too_large"))
        }
        Err(ReadError::Io) => Err(HttpIngestError::Protocol("invalid_compression")),
    }
}

enum ReadError {
    TooLarge,
    Io,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    maximum: usize,
) -> Result<Vec<u8>, ReadError> {
    let mut output = Vec::with_capacity(maximum.min(64 * 1024));
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut chunk).await.map_err(|_| ReadError::Io)?;
        if count == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(count) > maximum {
            return Err(ReadError::TooLarge);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

#[derive(Debug)]
enum HttpIngestError {
    Protocol(&'static str),
    TooLarge(&'static str),
    UnsupportedEncoding,
    RateLimited,
    Application(IngestError),
}

impl HttpIngestError {
    const fn outcome(&self) -> IngestOutcome {
        match self {
            Self::Protocol(_) | Self::UnsupportedEncoding => IngestOutcome {
                kind: IngestOutcomeKind::Invalid,
                reason: "invalid_request",
                quantity: 1,
            },
            Self::TooLarge(_) => IngestOutcome {
                kind: IngestOutcomeKind::TooLarge,
                reason: "request_too_large",
                quantity: 1,
            },
            Self::RateLimited => IngestOutcome {
                kind: IngestOutcomeKind::RateLimited,
                reason: "parsing_capacity",
                quantity: 1,
            },
            Self::Application(error) => IngestOutcome {
                kind: match error.kind() {
                    IngestErrorKind::Invalid | IngestErrorKind::ScrubFailed => {
                        IngestOutcomeKind::Invalid
                    }
                    IngestErrorKind::Unauthorized => IngestOutcomeKind::Filtered,
                    IngestErrorKind::TooLarge => IngestOutcomeKind::TooLarge,
                    IngestErrorKind::RateLimited => IngestOutcomeKind::RateLimited,
                    IngestErrorKind::Unavailable
                    | IngestErrorKind::Timeout
                    | IngestErrorKind::ShuttingDown => IngestOutcomeKind::StorageUnavailable,
                },
                reason: error.code(),
                quantity: 1,
            },
        }
    }
}

impl From<ProtocolError> for HttpIngestError {
    fn from(error: ProtocolError) -> Self {
        match error.kind() {
            ProtocolErrorKind::Invalid => Self::Protocol(error.code()),
            ProtocolErrorKind::TooLarge => Self::TooLarge(error.code()),
        }
    }
}

impl From<IngestError> for HttpIngestError {
    fn from(error: IngestError) -> Self {
        Self::Application(error)
    }
}

fn success_response(result: IngestResult, backoff_seconds: u64) -> Response {
    let mut response = (
        StatusCode::OK,
        Json(SuccessResponse {
            id: result.event_id.map(|event_id| event_id.to_string()),
        }),
    )
        .into_response();
    if !result.disabled_categories.is_empty() {
        let value = format!(
            "{}:{}:project:feature_disabled",
            backoff_seconds,
            result.disabled_categories.join(";")
        );
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert("x-sentry-rate-limits", value);
        }
    }
    response
}

fn minidump_success_response(result: IngestResult, backoff_seconds: u64) -> Response {
    let mut response = (
        StatusCode::OK,
        result
            .event_id
            .map_or_else(String::new, |event_id| event_id.to_string()),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if !result.disabled_categories.is_empty() {
        let value = format!(
            "{}:{}:project:feature_disabled",
            backoff_seconds,
            result.disabled_categories.join(";")
        );
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert("x-sentry-rate-limits", value);
        }
    }
    response
}

fn map_http_error(request_id: RequestId, error: HttpIngestError) -> Response {
    match error {
        HttpIngestError::Protocol(code) => error_response(
            request_id,
            StatusCode::BAD_REQUEST,
            code,
            "invalid request",
            None,
        ),
        HttpIngestError::TooLarge(code) => error_response(
            request_id,
            StatusCode::PAYLOAD_TOO_LARGE,
            code,
            "payload too large",
            None,
        ),
        HttpIngestError::UnsupportedEncoding => error_response(
            request_id,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_encoding",
            "unsupported content encoding",
            None,
        ),
        HttpIngestError::RateLimited => error_response(
            request_id,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "request capacity exceeded",
            Some(1),
        ),
        HttpIngestError::Application(error) => {
            let (status, message, retry) = match error.kind() {
                IngestErrorKind::Invalid => (StatusCode::BAD_REQUEST, "invalid request", None),
                IngestErrorKind::ScrubFailed => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily unavailable",
                    Some(1),
                ),
                IngestErrorKind::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", None),
                IngestErrorKind::TooLarge => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "payload too large", None)
                }
                IngestErrorKind::RateLimited => {
                    (StatusCode::TOO_MANY_REQUESTS, "rate limited", Some(1))
                }
                IngestErrorKind::Unavailable
                | IngestErrorKind::Timeout
                | IngestErrorKind::ShuttingDown => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily unavailable",
                    Some(1),
                ),
            };
            error_response(request_id, status, error.code(), message, retry)
        }
    }
}

fn error_response(
    request_id: RequestId,
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retry_after: Option<u64>,
) -> Response {
    let mut response = (
        status,
        Json(ErrorEnvelope {
            error: ErrorBody {
                code,
                message,
                request_id: request_id.to_string(),
            },
        }),
    )
        .into_response();
    if let Some(seconds) = retry_after {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds.to_string()).expect("integer is a valid header"),
        );
    }
    response
}
