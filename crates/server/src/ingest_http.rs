use std::{
    io,
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
use faultkeep_application::{
    ingest::{
        DisabledCategory, DiscardedItem, IngestError, IngestErrorKind, IngestRequest, IngestResult,
        IngestService, PrimaryEvent,
    },
    observability::{Metric, Metrics, Outcome, RequestId},
    shutdown::ShutdownSignal,
};
use faultkeep_domain::{DsnKey, ProjectId};
use faultkeep_ports::{IngestOutcome, IngestOutcomeKind};
use faultkeep_sentry_protocol::{
    EnvelopeLimits, ParsedEnvelope, ProtocolError, ProtocolErrorKind, parse_envelope,
    parse_query_auth, parse_store_event, parse_x_sentry_auth,
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
        .with_state(state)
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
        parse_envelope(
            &decoded,
            EnvelopeLimits {
                max_items: state.config.max_envelope_items,
                max_event_bytes: state.config.max_event_bytes,
            },
        )?
    } else {
        ParsedEnvelope {
            event_id: None,
            dsn: None,
            primary: Some(parse_store_event(&decoded, state.config.max_event_bytes)?),
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

const fn map_category(category: faultkeep_sentry_protocol::DisabledCategory) -> DisabledCategory {
    use faultkeep_sentry_protocol::DisabledCategory as Wire;
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
