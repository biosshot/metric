use std::{error::Error, path::PathBuf, process::Command, sync::Arc, time::Duration};

use faultkeep_application::{
    ingest::{AttachmentIngestConfig, IngestService},
    observability::Metrics,
    shutdown::ShutdownRoot,
};
use faultkeep_blob::{LocalBlobConfig, LocalBlobStore};
use faultkeep_domain::{
    DsnKey, EventId, IpScrubPolicy, ItemCapabilities, ProjectAcceptanceState, ProjectId,
    ProjectIngestLimits, ProjectKeyState, ProjectSnapshot, ScrubPolicy, SecretBytes, Timestamp,
};
use faultkeep_ports::BlobStore;
use faultkeep_server::{config::IngestConfig, http, ingest_http};
use faultkeep_testkit::{
    FakeEventSink, FakeOutcomeSink, FakeProjectResolver, FixedClock, FixedRandom,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpListener;

const KEY_TEXT: &str = "0123456789abcdef0123456789abcdef";
const NODE_SDK_VERSION: &str = "10.66.0";

#[derive(Deserialize)]
struct SenderResult {
    event_id: String,
    flushed: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node.js and npm ci in sdk-tests/node"]
async fn real_node_sdk_sends_an_error_event() {
    exercise_real_node_sdk().await.unwrap();
}

async fn exercise_real_node_sdk() -> Result<(), Box<dyn Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sender = workspace.join("sdk-tests/node/send-error.mjs");
    let installed_sdk = workspace.join("sdk-tests/node/node_modules/@sentry/node/package.json");
    if !installed_sdk.is_file() {
        return Err("run npm ci in sdk-tests/node before the real SDK gate".into());
    }

    let root = ShutdownRoot::new();
    let sink = FakeEventSink::accepting();
    let (app, blob, blob_directory) = test_app(sink.clone(), &root).await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(http::run(
        listener,
        root.signal(),
        Duration::from_secs(2),
        app,
    ));
    let dsn = format!("http://{KEY_TEXT}@{address}/42");

    let sdk_process =
        tokio::task::spawn_blocking(move || Command::new("node").arg(sender).arg(dsn).output())
            .await;
    let output = sdk_process??;
    if output.status.success() {
        for _ in 0..40 {
            if !sink.events().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    root.begin();
    let server_result = server.await;
    server_result??;

    if !output.status.success() {
        return Err(format!(
            "real Node SDK exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let sender_result: SenderResult = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "could not parse Node SDK result ({error}); stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if !sender_result.flushed {
        return Err("real Node SDK reported an incomplete flush".into());
    }

    let events = sink.events();
    if events.len() != 1 {
        return Err(format!(
            "expected one accepted Error Event, got {}; SDK stderr: {}",
            events.len(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let event = &events[0];
    if event.event_id != EventId::parse(&sender_result.event_id)? {
        return Err("SDK-reported Event ID differs from the accepted Event ID".into());
    }

    let payload: Value = serde_json::from_slice(event.payload.as_bytes())?;
    if payload.pointer("/sdk/name").and_then(Value::as_str) != Some("sentry.javascript.node")
        || payload.pointer("/sdk/version").and_then(Value::as_str) != Some(NODE_SDK_VERSION)
    {
        return Err("accepted Event does not contain the pinned Node SDK metadata".into());
    }
    if payload.get("environment").and_then(Value::as_str) != Some("sdk-compatibility")
        || payload.get("release").and_then(Value::as_str) != Some("faultkeep-node-sdk-test@1.0.0")
        || payload
            .pointer("/exception/values/0/type")
            .and_then(Value::as_str)
            != Some("FaultkeepSdkCompatibilityError")
        || payload
            .pointer("/exception/values/0/value")
            .and_then(Value::as_str)
            != Some("Faultkeep real Node SDK compatibility event")
    {
        return Err("accepted Event lost the Node SDK compatibility fixture fields".into());
    }
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
    let key = faultkeep_domain::blob::BlobKey::new(
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
    drop(reader);
    std::fs::remove_dir_all(blob_directory)?;
    Ok(())
}

async fn test_app(
    sink: FakeEventSink,
    root: &ShutdownRoot,
) -> (axum::Router, LocalBlobStore, PathBuf) {
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
