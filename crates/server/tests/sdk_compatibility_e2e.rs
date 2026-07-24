use std::{error::Error, path::PathBuf, process::Command, sync::Arc, time::Duration};

use axum::{
    Router,
    http::header,
    response::{Html, IntoResponse},
    routing::get,
};
use faultkeep_application::{
    ingest::{AttachmentIngestConfig, IngestService},
    observability::Metrics,
    shutdown::ShutdownRoot,
};
use faultkeep_blob::{LocalBlobConfig, LocalBlobStore};
use faultkeep_domain::{
    DsnKey, EventId, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState, ProjectId,
    ProjectIngestLimits, ProjectKeyState, ProjectSnapshot, ScrubPolicy, SecretBytes, Timestamp,
    blob::BlobKey,
};
use faultkeep_ports::BlobStore;
use faultkeep_server::{config::IngestConfig, http, ingest_http};
use faultkeep_testkit::{
    FakeEventSink, FakeOutcomeSink, FakeProjectResolver, FixedClock, FixedRandom,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::{net::TcpListener, task::JoinHandle};

const KEY_TEXT: &str = "0123456789abcdef0123456789abcdef";
const NODE_SDK_VERSION: &str = "10.66.0";
const BROWSER_SDK_VERSION: &str = "10.66.0";

#[derive(Deserialize)]
struct SenderResult {
    event_id: String,
    flushed: bool,
}

struct RunningHarness {
    root: ShutdownRoot,
    sink: FakeEventSink,
    blob: LocalBlobStore,
    blob_directory: PathBuf,
    address: std::net::SocketAddr,
    server: JoinHandle<Result<(), std::io::Error>>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node.js and npm ci in sdk-tests/node"]
async fn real_node_sdk_sends_an_error_event_without_blob() {
    exercise_real_node_sdk("send-error.mjs", NodeFixture::ErrorOnly)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node.js and npm ci in sdk-tests/node"]
async fn real_node_sdk_sends_an_attachment_event() {
    exercise_real_node_sdk("send-attachment.mjs", NodeFixture::Attachment)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires npm ci/build in sdk-tests/browser and an installed Chromium"]
async fn real_browser_sdk_sends_an_error_event() {
    exercise_real_browser_sdk().await.unwrap();
}

#[derive(Clone, Copy)]
enum NodeFixture {
    ErrorOnly,
    Attachment,
}

async fn exercise_real_node_sdk(
    script_name: &'static str,
    fixture: NodeFixture,
) -> Result<(), Box<dyn Error>> {
    let workspace = workspace();
    let sender = workspace.join("sdk-tests/node").join(script_name);
    let installed_sdk = workspace.join("sdk-tests/node/node_modules/@sentry/node/package.json");
    if !installed_sdk.is_file() {
        return Err("run npm ci in sdk-tests/node before the real SDK gate".into());
    }

    let harness = start_harness(Router::new()).await?;
    let dsn = format!("http://{KEY_TEXT}@{}/42", harness.address);
    let output =
        tokio::task::spawn_blocking(move || Command::new("node").arg(sender).arg(dsn).output())
            .await;
    wait_for_sink(&harness.sink, output.as_ref().is_ok_and(Result::is_ok)).await;

    let verification: Result<(), Box<dyn Error>> = async {
        let output = output??;
        let sender_result = verify_process_output(&output, "Node")?;
        let event = one_event(&harness.sink, &sender_result, "Node", &output.stderr)?;
        let payload: Value = serde_json::from_slice(event.payload.as_bytes())?;
        verify_sdk(
            &payload,
            "sentry.javascript.node",
            NODE_SDK_VERSION,
            "faultkeep-node-sdk-test@1.0.0",
        )?;

        match fixture {
            NodeFixture::ErrorOnly => {
                verify_exception(
                    &payload,
                    "FaultkeepSdkCompatibilityError",
                    "Faultkeep real Node SDK compatibility event",
                )?;
                if payload.get("attachments").is_some() || harness.blob.capacity().used_bytes != 0 {
                    return Err("base Node Error Event unexpectedly created a blob".into());
                }
            }
            NodeFixture::Attachment => {
                verify_exception(
                    &payload,
                    "FaultkeepSdkAttachmentCompatibilityError",
                    "Faultkeep real Node SDK attachment compatibility event",
                )?;
                verify_attachment(&payload, &harness.blob).await?;
            }
        }
        Ok(())
    }
    .await;

    stop_harness(harness).await?;
    verification
}

async fn exercise_real_browser_sdk() -> Result<(), Box<dyn Error>> {
    let workspace = workspace();
    let browser_root = workspace.join("sdk-tests/browser");
    let runner = browser_root.join("run-browser.mjs");
    let page = browser_root.join("page.html");
    let bundle = browser_root.join("dist/send-error.js");
    let installed_sdk = browser_root.join("node_modules/@sentry/browser/package.json");
    if !installed_sdk.is_file() || !bundle.is_file() {
        return Err("run npm ci and npm run build in sdk-tests/browser before the gate".into());
    }

    let page_html = Arc::<str>::from(std::fs::read_to_string(page)?);
    let bundle_js = Arc::<str>::from(std::fs::read_to_string(bundle)?);
    let browser_routes = Router::new()
        .route(
            "/sdk-browser",
            get({
                let page_html = Arc::clone(&page_html);
                move || {
                    let page_html = Arc::clone(&page_html);
                    async move { Html(page_html.to_string()) }
                }
            }),
        )
        .route(
            "/sdk-browser.js",
            get({
                let bundle_js = Arc::clone(&bundle_js);
                move || {
                    let bundle_js = Arc::clone(&bundle_js);
                    async move {
                        (
                            [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                            bundle_js.to_string(),
                        )
                            .into_response()
                    }
                }
            }),
        );
    let harness = start_harness(browser_routes).await?;
    let page_url = format!("http://{}/sdk-browser", harness.address);
    let dsn = format!("http://{KEY_TEXT}@{}/42", harness.address);
    let output = tokio::task::spawn_blocking(move || {
        Command::new("node")
            .arg(runner)
            .arg(page_url)
            .arg(dsn)
            .output()
    })
    .await;
    wait_for_sink(&harness.sink, output.as_ref().is_ok_and(Result::is_ok)).await;

    let verification = (|| -> Result<(), Box<dyn Error>> {
        let output = output??;
        let sender_result = verify_process_output(&output, "Browser")?;
        let event = one_event(&harness.sink, &sender_result, "Browser", &output.stderr)?;
        let payload: Value = serde_json::from_slice(event.payload.as_bytes())?;
        verify_sdk(
            &payload,
            "sentry.javascript.browser",
            BROWSER_SDK_VERSION,
            "faultkeep-browser-sdk-test@1.0.0",
        )?;
        verify_exception(
            &payload,
            "FaultkeepBrowserSdkCompatibilityError",
            "Faultkeep real Browser SDK compatibility event",
        )?;
        if payload.get("attachments").is_some() || harness.blob.capacity().used_bytes != 0 {
            return Err("base Browser Error Event unexpectedly created a blob".into());
        }
        Ok(())
    })();

    stop_harness(harness).await?;
    verification
}

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

async fn start_harness(extra_routes: Router) -> Result<RunningHarness, Box<dyn Error>> {
    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let (app, blob, blob_directory) = test_app(sink.clone(), &root).await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(http::run(
        listener,
        root.signal(),
        Duration::from_secs(2),
        app.merge(extra_routes),
    ));
    Ok(RunningHarness {
        root,
        sink,
        blob,
        blob_directory,
        address,
        server,
    })
}

async fn stop_harness(harness: RunningHarness) -> Result<(), Box<dyn Error>> {
    harness.root.begin();
    let server_result = harness.server.await;
    let remove_result = std::fs::remove_dir_all(&harness.blob_directory);
    server_result??;
    remove_result?;
    Ok(())
}

async fn wait_for_sink(sink: &FakeEventSink, process_started: bool) {
    if !process_started {
        return;
    }
    for _ in 0..80 {
        if !sink.events().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn verify_process_output(
    output: &std::process::Output,
    runtime: &str,
) -> Result<SenderResult, Box<dyn Error>> {
    if !output.status.success() {
        return Err(format!(
            "real {runtime} SDK exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let result: SenderResult = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "could not parse {runtime} SDK result ({error}); stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if !result.flushed {
        return Err(format!("real {runtime} SDK reported an incomplete flush").into());
    }
    Ok(result)
}

fn one_event(
    sink: &FakeEventSink,
    sender_result: &SenderResult,
    runtime: &str,
    stderr: &[u8],
) -> Result<faultkeep_domain::AcceptedEvent, Box<dyn Error>> {
    let events = sink.events();
    if events.len() != 1 {
        return Err(format!(
            "expected one accepted {runtime} Error Event, got {}; SDK stderr: {}",
            events.len(),
            String::from_utf8_lossy(stderr)
        )
        .into());
    }
    let event = events
        .into_iter()
        .next()
        .ok_or("accepted Event disappeared after count validation")?;
    if event.event_id != EventId::parse(&sender_result.event_id)? {
        return Err(format!("{runtime} SDK Event ID differs from the accepted Event ID").into());
    }
    Ok(event)
}

fn verify_sdk(
    payload: &Value,
    name: &str,
    version: &str,
    release: &str,
) -> Result<(), Box<dyn Error>> {
    if payload.pointer("/sdk/name").and_then(Value::as_str) != Some(name)
        || payload.pointer("/sdk/version").and_then(Value::as_str) != Some(version)
        || payload.get("environment").and_then(Value::as_str) != Some("sdk-compatibility")
        || payload.get("release").and_then(Value::as_str) != Some(release)
    {
        return Err("accepted Event lost the pinned SDK metadata".into());
    }
    Ok(())
}

fn verify_exception(
    payload: &Value,
    exception_type: &str,
    exception_value: &str,
) -> Result<(), Box<dyn Error>> {
    if payload
        .pointer("/exception/values/0/type")
        .and_then(Value::as_str)
        != Some(exception_type)
        || payload
            .pointer("/exception/values/0/value")
            .and_then(Value::as_str)
            != Some(exception_value)
    {
        return Err("accepted Event lost the compatibility exception fixture".into());
    }
    Ok(())
}

async fn verify_attachment(payload: &Value, blob: &LocalBlobStore) -> Result<(), Box<dyn Error>> {
    let attachment = payload
        .get("attachments")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or("real Node SDK attachment metadata is missing")?;
    if attachment.get("filename").and_then(Value::as_str) != Some("faultkeep-context.json")
        || attachment.get("content_type").and_then(Value::as_str) != Some("application/json")
    {
        return Err("real Node SDK attachment metadata is incompatible".into());
    }
    let key = BlobKey::new(
        attachment
            .get("blob_key")
            .and_then(Value::as_str)
            .ok_or("attachment blob key is missing")?
            .to_owned(),
    )?;
    let mut reader = blob.open(&key).await?;
    let bytes = reader
        .read_chunk(1024)
        .await?
        .ok_or("attachment blob is empty")?;
    if serde_json::from_slice::<Value>(&bytes)?
        != serde_json::json!({"safe": true, "source": "node-sdk"})
    {
        return Err("real Node SDK attachment bytes changed unexpectedly".into());
    }
    Ok(())
}

async fn test_app(sink: FakeEventSink, root: &ShutdownRoot) -> (Router, LocalBlobStore, PathBuf) {
    let config = IngestConfig {
        max_compressed_request_bytes: 20 * 1024 * 1024,
        max_decompressed_request_bytes: 100 * 1024 * 1024,
        max_event_bytes: 1024 * 1024,
        max_envelope_items: 100,
        max_active_requests: 16,
        max_parsing_tasks: 2,
        max_waiting_for_storage: 16,
        request_timeout: "10s".parse().unwrap(),
        unsupported_backoff_seconds: 3600,
        project_cache: Default::default(),
        batch: Default::default(),
        event_codec: Default::default(),
        backlog: Default::default(),
        attachments: Default::default(),
    };
    let directory =
        std::env::temp_dir().join(format!("faultkeep-sdk-blob-{}", uuid::Uuid::new_v4()));
    let blob = LocalBlobStore::new(
        &directory,
        LocalBlobConfig {
            capacity_bytes: 1024 * 1024 + 128,
            reserve_bytes: 128,
            max_object_bytes: 1024 * 1024,
        },
    )
    .await
    .unwrap();
    let service = Arc::new(
        IngestService::new(
            Arc::new(FakeProjectResolver::new(
                DsnKey::parse(KEY_TEXT).unwrap(),
                ProjectSnapshot {
                    project_id: ProjectId::new(42).unwrap(),
                    state: ProjectAcceptanceState::Active,
                    key_state: ProjectKeyState::Active,
                    scrub_policy: ScrubPolicy {
                        revision: 1,
                        ip_policy: IpScrubPolicy::Remove,
                        hmac_key: SecretBytes::new([9; 32]),
                    },
                    items: ItemCapabilities {
                        error: true,
                        client_report: true,
                    },
                    limits: ProjectIngestLimits::default(),
                    grouping_revision: 1,
                },
            )),
            Arc::new(sink),
            Arc::new(FakeOutcomeSink::default()),
            Arc::new(FixedClock(Timestamp::from_unix_millis(0).unwrap())),
            Arc::new(FixedRandom(7)),
            config.max_waiting_for_storage,
            root.signal(),
        )
        .with_blob_store(Arc::new(blob.clone()), AttachmentIngestConfig::default()),
    );
    (
        http::router(
            root.signal(),
            Metrics,
            ingest_http::router(service, config, root.signal()),
        ),
        blob,
        directory,
    )
}
