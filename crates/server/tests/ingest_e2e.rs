use std::{io::Write, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use faultkeep_application::{
    ingest::IngestService, observability::Metrics, shutdown::ShutdownRoot,
};
use faultkeep_domain::{
    DsnKey, IpScrubPolicy, ItemCapabilities, ProjectId, ProjectSnapshot, ScrubPolicy, SecretBytes,
    Timestamp,
};
use faultkeep_ports::DurableOutcome;
use faultkeep_server::{config::IngestConfig, http, ingest_http};
use faultkeep_testkit::{
    FakeEventSink, FakeOutcomeSink, FakeProjectResolver, FixedClock, FixedRandom,
};
use flate2::{
    Compression,
    write::{GzEncoder, ZlibEncoder},
};
use tower::ServiceExt;

const KEY_TEXT: &str = "0123456789abcdef0123456789abcdef";
const EVENT: &str = include_str!("fixtures/error-event-v1.json");
const PYTHON_EVENT: &str = include_str!("fixtures/python-2.32.0-error-event-v1.json");

fn config() -> IngestConfig {
    IngestConfig {
        max_compressed_request_bytes: 20 * 1024 * 1024,
        max_decompressed_request_bytes: 100 * 1024 * 1024,
        max_event_bytes: 1024 * 1024,
        max_envelope_items: 100,
        max_active_requests: 512,
        max_parsing_tasks: 2,
        max_waiting_for_storage: 512,
        request_timeout: "10s".parse().unwrap(),
        unsupported_backoff_seconds: 3600,
    }
}

fn snapshot() -> ProjectSnapshot {
    ProjectSnapshot {
        project_id: ProjectId::new(42).unwrap(),
        scrub_policy: ScrubPolicy {
            revision: 1,
            ip_policy: IpScrubPolicy::Hmac,
            hmac_key: SecretBytes::new([9; 32]),
        },
        items: ItemCapabilities {
            error: true,
            client_report: true,
        },
    }
}

fn test_app(config: IngestConfig, sink: FakeEventSink, root: &ShutdownRoot) -> Router {
    let service = Arc::new(IngestService::new(
        Arc::new(FakeProjectResolver::new(
            DsnKey::parse(KEY_TEXT).unwrap(),
            snapshot(),
        )),
        Arc::new(sink),
        Arc::new(FakeOutcomeSink::default()),
        Arc::new(FixedClock(Timestamp::from_unix_millis(0).unwrap())),
        Arc::new(FixedRandom(7)),
        config.max_waiting_for_storage,
        root.signal(),
    ));
    http::router(
        root.signal(),
        Metrics,
        ingest_http::router(service, config, root.signal()),
    )
}

fn envelope(extra_items: &str) -> String {
    format!(
        "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{}{}",
        EVENT.len(),
        EVENT,
        extra_items
    )
}

fn request(body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/42/envelope/")
        .header(
            "x-sentry-auth",
            format!("Sentry sentry_version=7,sentry_key={KEY_TEXT}"),
        )
        .body(body.into())
        .unwrap()
}

#[tokio::test]
async fn valid_envelope_is_scrubbed_before_fake_durable_acceptance() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let response = test_app(config(), sink.clone(), &root)
        .oneshot(request(envelope("")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stored = sink.events();
    assert_eq!(stored.len(), 1);
    let payload = String::from_utf8(stored[0].payload.as_bytes().to_vec()).unwrap();
    assert!(!payload.contains("fixture-secret"));
    assert!(!payload.contains("fixture-api-key"));
    assert!(!payload.contains("192.0.2.10"));
}

#[tokio::test]
async fn captured_official_python_sdk_2_32_fixture_is_accepted() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let body = format!(
        "{{\"event_id\":\"aa40a14691564910ae6eb2affdba35f9\"}}\n{{\"type\":\"event\",\"content_type\":\"application/json\",\"length\":{}}}\n{}",
        PYTHON_EVENT.len(),
        PYTHON_EVENT
    );
    let response = test_app(config(), sink.clone(), &root)
        .oneshot(request(body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(sink.events().len(), 1);
}

#[tokio::test]
async fn mixed_disabled_item_preserves_error_and_returns_category_backoff() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let response = test_app(config(), sink.clone(), &root)
        .oneshot(request(envelope(
            "\n{\"type\":\"transaction\",\"length\":2}\n{}",
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(sink.events().len(), 1);
    assert!(
        response.headers()["x-sentry-rate-limits"]
            .to_str()
            .unwrap()
            .contains("transaction")
    );
}

#[tokio::test]
async fn unsupported_only_envelope_is_handled_without_durable_event() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let body = "{}\n{\"type\":\"session\",\"length\":2}\n{}";
    let response = test_app(config(), sink.clone(), &root)
        .oneshot(request(body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn malformed_or_oversized_envelopes_fail_without_sink_call() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let app = test_app(config(), sink.clone(), &root);
    let malformed = app
        .clone()
        .oneshot(request("{}\n{\"type\":\"event\",\"length\":99}\n{}"))
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    let mut small = config();
    small.max_event_bytes = 16;
    let oversized = test_app(small, sink.clone(), &root)
        .oneshot(request(envelope("")))
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn compressed_bytes_and_item_count_have_independent_bounds() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();

    let mut compressed_limited = config();
    compressed_limited.max_compressed_request_bytes = 16;
    let response = test_app(compressed_limited, sink.clone(), &root)
        .oneshot(request(envelope("")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let mut item_limited = config();
    item_limited.max_envelope_items = 1;
    let response = test_app(item_limited, sink.clone(), &root)
        .oneshot(request(envelope(
            "\n{\"type\":\"client_report\",\"length\":23}\n{\"discarded_events\":[]}",
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn gzip_and_deflate_are_stream_decoded() {
    let root = ShutdownRoot::new();
    for (encoding, body) in [
        ("gzip", gzip(envelope("").as_bytes())),
        ("deflate", deflate(envelope("").as_bytes())),
    ] {
        let sink = FakeEventSink::accepting();
        let mut compressed = request(body);
        compressed
            .headers_mut()
            .insert(header::CONTENT_ENCODING, encoding.parse().unwrap());
        let response = test_app(config(), sink.clone(), &root)
            .oneshot(compressed)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{encoding}");
        assert_eq!(sink.events().len(), 1, "{encoding}");
    }
}

#[tokio::test]
async fn decompression_limit_and_slow_storage_deadline_are_bounded() {
    let root = ShutdownRoot::new();
    let mut limited = config();
    limited.max_decompressed_request_bytes = 64;
    let mut compressed = request(gzip(envelope("").as_bytes()));
    compressed
        .headers_mut()
        .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
    let response = test_app(limited, FakeEventSink::accepting(), &root)
        .oneshot(compressed)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let mut timed = config();
    timed.request_timeout = "10ms".parse().unwrap();
    let response = test_app(
        timed,
        FakeEventSink::with_delay(Duration::from_millis(100)),
        &root,
    )
    .oneshot(request(envelope("")))
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn shutdown_fence_rejects_before_durable_work() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let app = test_app(config(), sink.clone(), &root);
    root.begin();
    let response = app.oneshot(request(envelope(""))).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(sink.events().is_empty());
}

#[tokio::test]
async fn error_response_never_echoes_payload_secret() {
    let root = ShutdownRoot::new();
    let response = test_app(config(), FakeEventSink::accepting(), &root)
        .oneshot(request("payload-secret"))
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    assert!(
        !String::from_utf8(body.to_vec())
            .unwrap()
            .contains("payload-secret")
    );
}

#[tokio::test]
async fn auth_variants_and_project_consistency_are_enforced() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let dsn_body = format!(
        "{{\"dsn\":\"https://{KEY_TEXT}@example.invalid/42\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
        EVENT.len(),
        EVENT
    );
    let dsn_request = Request::builder()
        .method("POST")
        .uri("/api/42/envelope/")
        .body(Body::from(dsn_body))
        .unwrap();
    let response = test_app(config(), sink.clone(), &root)
        .oneshot(dsn_request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let query_request = Request::builder()
        .method("POST")
        .uri(format!("/api/42/store/?sentry_key={KEY_TEXT}"))
        .body(Body::from(EVENT))
        .unwrap();
    let response = test_app(config(), sink.clone(), &root)
        .oneshot(query_request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let conflict = format!(
        "{{\"dsn\":\"https://{KEY_TEXT}@example.invalid/43\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{}",
        EVENT.len(),
        EVENT
    );
    let conflict_request = Request::builder()
        .method("POST")
        .uri("/api/42/envelope/")
        .body(Body::from(conflict))
        .unwrap();
    let response = test_app(config(), sink, &root)
        .oneshot(conflict_request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn duplicate_is_success_and_capacity_exhaustion_is_bounded() {
    let root = ShutdownRoot::new();
    let response = test_app(
        config(),
        FakeEventSink::with_outcome(Ok(DurableOutcome::Duplicate)),
        &root,
    )
    .oneshot(request(envelope("")))
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut one_request = config();
    one_request.max_active_requests = 1;
    let app = test_app(
        one_request,
        FakeEventSink::with_delay(Duration::from_millis(100)),
        &root,
    );
    let first = tokio::spawn(app.clone().oneshot(request(envelope(""))));
    tokio::time::sleep(Duration::from_millis(10)).await;
    let second = app.oneshot(request(envelope(""))).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn slow_stream_is_cancelled_by_request_deadline() {
    let root = ShutdownRoot::new();
    let mut timed = config();
    timed.request_timeout = "10ms".parse().unwrap();
    let body = Body::from_stream(futures_util::stream::pending::<
        Result<bytes::Bytes, std::io::Error>,
    >());
    let response = test_app(timed, FakeEventSink::accepting(), &root)
        .oneshot(request(body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn dropped_client_request_cancels_before_the_durable_sink() {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let body = Body::from_stream(futures_util::stream::pending::<
        Result<bytes::Bytes, std::io::Error>,
    >());
    let request_task = tokio::spawn(test_app(config(), sink.clone(), &root).oneshot(request(body)));
    tokio::task::yield_now().await;
    request_task.abort();
    assert!(request_task.await.unwrap_err().is_cancelled());
    assert!(sink.events().is_empty());
}

fn gzip(input: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(input).unwrap();
    encoder.finish().unwrap()
}

fn deflate(input: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(input).unwrap();
    encoder.finish().unwrap()
}
