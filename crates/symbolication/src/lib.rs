//! External Symbolicator HTTP adapter and private-source token contract.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use metric_domain::{
    ProjectId,
    symbolication::{
        BackendSymbolicationResult, BackendSymbolicationStatus, RawTraceOrigin, SymbolicatedFrame,
        SymbolicatedStacktrace, SymbolicationDiagnosticCode, SymbolicationKind,
        SymbolicationRequest,
    },
};
use metric_ports::{PortFuture, SymbolicationBackend, SymbolicationBackendError};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct PrivateSourceSigner {
    current: Arc<[u8]>,
    previous: Option<Arc<[u8]>>,
}

impl PrivateSourceSigner {
    pub fn new(current: Vec<u8>, previous: Option<Vec<u8>>) -> Result<Self, &'static str> {
        if current.len() < 32 || previous.as_ref().is_some_and(|value| value.len() < 32) {
            return Err("private source HMAC keys must contain at least 32 bytes");
        }
        Ok(Self {
            current: current.into(),
            previous: previous.map(Into::into),
        })
    }

    #[must_use]
    pub fn token(&self, project_id: ProjectId) -> String {
        token_with(&self.current, project_id, PrivateSourceKind::DebugFile)
    }

    #[must_use]
    pub fn verify(&self, project_id: ProjectId, token: &str) -> bool {
        verify_with(
            &self.current,
            project_id,
            token,
            PrivateSourceKind::DebugFile,
        ) || self
            .previous
            .as_ref()
            .is_some_and(|key| verify_with(key, project_id, token, PrivateSourceKind::DebugFile))
    }

    #[must_use]
    pub fn artifact_token(&self, project_id: ProjectId) -> String {
        token_with(&self.current, project_id, PrivateSourceKind::ArtifactBundle)
    }

    #[must_use]
    pub fn verify_artifact(&self, project_id: ProjectId, token: &str) -> bool {
        verify_with(
            &self.current,
            project_id,
            token,
            PrivateSourceKind::ArtifactBundle,
        ) || self.previous.as_ref().is_some_and(|key| {
            verify_with(key, project_id, token, PrivateSourceKind::ArtifactBundle)
        })
    }
}

#[derive(Debug, Clone)]
pub struct ExternalSymbolicatorConfig {
    pub endpoint: Url,
    pub callback_base_url: Url,
    pub request_timeout: Duration,
    pub maximum_concurrency: usize,
    pub circuit_failure_threshold: u32,
    pub circuit_cooldown: Duration,
    pub maximum_response_bytes: usize,
}

pub struct ExternalSymbolicator {
    client: Client,
    config: ExternalSymbolicatorConfig,
    signer: PrivateSourceSigner,
    concurrency: Semaphore,
    consecutive_failures: AtomicU32,
    circuit_open_until_millis: AtomicU64,
}

impl ExternalSymbolicator {
    pub fn new(
        config: ExternalSymbolicatorConfig,
        signer: PrivateSourceSigner,
    ) -> Result<Self, &'static str> {
        if config.endpoint.scheme() != "http" && config.endpoint.scheme() != "https"
            || config.callback_base_url.scheme() != "http"
                && config.callback_base_url.scheme() != "https"
            || config.request_timeout.is_zero()
            || config.request_timeout > Duration::from_secs(120)
            || !(1..=1024).contains(&config.maximum_concurrency)
            || !(1..=1000).contains(&config.circuit_failure_threshold)
            || config.circuit_cooldown.is_zero()
            || !(1024..=16 * 1024 * 1024).contains(&config.maximum_response_bytes)
        {
            return Err("external Symbolicator configuration is invalid");
        }
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| "external Symbolicator HTTP client is invalid")?;
        let maximum_concurrency = config.maximum_concurrency;
        Ok(Self {
            client,
            config,
            signer,
            concurrency: Semaphore::new(maximum_concurrency),
            consecutive_failures: AtomicU32::new(0),
            circuit_open_until_millis: AtomicU64::new(0),
        })
    }

    async fn execute(
        &self,
        request: SymbolicationRequest,
    ) -> Result<BackendSymbolicationResult, SymbolicationBackendError> {
        if now_millis() < self.circuit_open_until_millis.load(Ordering::Acquire) {
            return Err(SymbolicationBackendError::Unavailable);
        }
        let _permit = self
            .concurrency
            .acquire()
            .await
            .map_err(|_| SymbolicationBackendError::Unavailable)?;
        let project_id = request.project_id;
        let origins = request
            .traces
            .iter()
            .map(|trace| trace.origin)
            .collect::<Vec<_>>();
        let (endpoint, payload) = match request.kind {
            SymbolicationKind::JavaScript => {
                let source_url = self
                    .config
                    .callback_base_url
                    .join(&format!(
                        "internal/symbolicator/projects/{}/artifact-lookup/?revision={}",
                        request.project_id.get(),
                        request.artifact_revision,
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?;
                let mut endpoint = self.config.endpoint.clone();
                if endpoint.path().ends_with("/symbolicate") {
                    let path = endpoint.path().trim_end_matches("/symbolicate").to_owned()
                        + "/symbolicate-js";
                    endpoint.set_path(&path);
                } else {
                    endpoint.set_path("/symbolicate-js");
                }
                endpoint
                    .query_pairs_mut()
                    .append_pair("scope", &format!("project-{}", project_id.get()));
                (
                    endpoint,
                    serde_json::to_value(JsWireRequest::from_domain(
                        request,
                        WireSource {
                            id: format!("private-js-project-{}", project_id.get()),
                            ty: "sentry",
                            url: source_url.as_str().to_owned(),
                            token: self.signer.artifact_token(project_id),
                        },
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?,
                )
            }
            SymbolicationKind::Native => {
                let source_url = self
                    .config
                    .callback_base_url
                    .join(&format!(
                        "internal/symbolicator/projects/{}/debug-files/?revision={}",
                        request.project_id.get(),
                        request.debug_file_revision,
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?;
                (
                    self.config.endpoint.clone(),
                    serde_json::to_value(WireRequest::from_domain(
                        request,
                        WireSource {
                            id: format!("private-project-{}", project_id.get()),
                            ty: "sentry",
                            url: source_url.as_str().to_owned(),
                            token: self.signer.token(project_id),
                        },
                    ))
                    .map_err(|_| SymbolicationBackendError::MalformedResponse)?,
                )
            }
            SymbolicationKind::NotRequired => {
                return Err(SymbolicationBackendError::MalformedResponse);
            }
        };
        let javascript = request_kind(&payload) != "native";
        let response = match self.client.post(endpoint).json(&payload).send().await {
            Ok(response) => response,
            Err(error) => {
                self.record_failure();
                return Err(classify_reqwest(error));
            }
        };
        if !response.status().is_success() {
            self.record_failure();
            return Err(if response.status().is_server_error() {
                SymbolicationBackendError::Unavailable
            } else {
                SymbolicationBackendError::MalformedResponse
            });
        }
        if response.content_length().is_some_and(|size| {
            usize::try_from(size).map_or(true, |size| size > self.config.maximum_response_bytes)
        }) {
            self.record_failure();
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::with_capacity(self.config.maximum_response_bytes.min(64 * 1024));
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.record_failure();
                    return Err(classify_reqwest(error));
                }
            };
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|size| size > self.config.maximum_response_bytes)
            {
                self.record_failure();
                return Err(SymbolicationBackendError::MalformedResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let result = if javascript {
            serde_json::from_slice::<JsWireResponse>(&bytes)
                .map_err(|_| SymbolicationBackendError::MalformedResponse)
                .and_then(|wire| wire.into_domain(&origins))
        } else {
            serde_json::from_slice::<WireResponse>(&bytes)
                .map_err(|_| SymbolicationBackendError::MalformedResponse)
                .and_then(|wire| wire.into_domain(&origins))
        }
        .inspect_err(|_| self.record_failure())?;
        self.consecutive_failures.store(0, Ordering::Release);
        Ok(result)
    }

    fn record_failure(&self) {
        let failures = self
            .consecutive_failures
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if failures >= self.config.circuit_failure_threshold {
            let cooldown =
                u64::try_from(self.config.circuit_cooldown.as_millis()).unwrap_or(u64::MAX);
            self.circuit_open_until_millis
                .store(now_millis().saturating_add(cooldown), Ordering::Release);
            self.consecutive_failures.store(0, Ordering::Release);
        }
    }
}

fn request_kind(payload: &serde_json::Value) -> &str {
    payload
        .get("platform")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

impl SymbolicationBackend for ExternalSymbolicator {
    fn symbolicate(
        &self,
        request: SymbolicationRequest,
    ) -> PortFuture<'_, Result<BackendSymbolicationResult, SymbolicationBackendError>> {
        Box::pin(self.execute(request))
    }
}

#[derive(Serialize)]
struct WireRequest {
    platform: &'static str,
    sources: [WireSource; 1],
    threads: Vec<WireTrace>,
    modules: Vec<WireModule>,
    options: WireOptions,
}

impl WireRequest {
    fn from_domain(request: SymbolicationRequest, source: WireSource) -> Self {
        Self {
            platform: "native",
            sources: [source],
            threads: request
                .traces
                .into_iter()
                .map(|trace| WireTrace {
                    frames: trace
                        .frames
                        .into_iter()
                        .map(|frame| WireFrame {
                            instruction_addr: frame.instruction_address.map(Into::into),
                            addr_mode: "abs",
                        })
                        .collect(),
                })
                .collect(),
            modules: request
                .modules
                .into_iter()
                .map(|module| WireModule {
                    ty: module.kind.map(Into::into),
                    debug_id: module.debug_id.map(Into::into),
                    code_id: module.code_id.map(Into::into),
                    debug_file: module.code_file.map(Into::into),
                    image_addr: module.image_address.map(Into::into),
                    image_size: module.image_size.map(|size| format!("0x{size:x}")),
                })
                .collect(),
            options: WireOptions {
                dif_candidates: true,
                apply_source_context: true,
                frame_order: "caller_first",
            },
        }
    }
}

#[derive(Serialize)]
struct WireSource {
    id: String,
    #[serde(rename = "type")]
    ty: &'static str,
    url: String,
    token: String,
}

#[derive(Serialize)]
struct WireTrace {
    frames: Vec<WireFrame>,
}

#[derive(Serialize)]
struct WireFrame {
    instruction_addr: Option<String>,
    addr_mode: &'static str,
}

#[derive(Serialize)]
struct WireModule {
    #[serde(rename = "type")]
    ty: Option<String>,
    debug_id: Option<String>,
    code_id: Option<String>,
    debug_file: Option<String>,
    image_addr: Option<String>,
    image_size: Option<String>,
}

#[derive(Serialize)]
struct WireOptions {
    dif_candidates: bool,
    apply_source_context: bool,
    frame_order: &'static str,
}

#[derive(Serialize)]
struct JsWireRequest {
    platform: &'static str,
    source: WireSource,
    stacktraces: Vec<JsWireTrace>,
    modules: Vec<JsWireModule>,
    release: Option<Box<str>>,
    dist: Option<Box<str>>,
    scraping: JsScraping,
    options: JsWireOptions,
}

impl JsWireRequest {
    fn from_domain(request: SymbolicationRequest, source: WireSource) -> Self {
        Self {
            platform: "javascript",
            source,
            stacktraces: request
                .traces
                .into_iter()
                .map(|trace| JsWireTrace {
                    frames: trace
                        .frames
                        .into_iter()
                        .map(|frame| JsWireFrame {
                            function: frame.function,
                            module: frame.module,
                            filename: frame.filename,
                            abs_path: frame.absolute_path,
                            lineno: frame.line,
                            colno: frame.column,
                            in_app: frame.in_app,
                        })
                        .collect(),
                })
                .collect(),
            modules: request
                .modules
                .into_iter()
                .filter_map(|module| {
                    Some(JsWireModule {
                        code_file: module.code_file?,
                        debug_id: module.debug_id?,
                        ty: "debug_id",
                    })
                })
                .collect(),
            release: request.release,
            dist: request.dist,
            scraping: JsScraping { enabled: false },
            options: JsWireOptions {
                apply_source_context: true,
                frame_order: "caller_first",
            },
        }
    }
}

#[derive(Serialize)]
struct JsWireTrace {
    frames: Vec<JsWireFrame>,
}

#[derive(Serialize)]
struct JsWireFrame {
    function: Option<Box<str>>,
    module: Option<Box<str>>,
    filename: Option<Box<str>>,
    abs_path: Option<Box<str>>,
    lineno: Option<u64>,
    colno: Option<u64>,
    in_app: Option<bool>,
}

#[derive(Serialize)]
struct JsWireModule {
    code_file: Box<str>,
    debug_id: Box<str>,
    #[serde(rename = "type")]
    ty: &'static str,
}

#[derive(Serialize)]
struct JsScraping {
    enabled: bool,
}

#[derive(Serialize)]
struct JsWireOptions {
    apply_source_context: bool,
    frame_order: &'static str,
}

#[derive(Deserialize)]
struct JsWireResponse {
    #[serde(default)]
    stacktraces: Vec<JsResponseTrace>,
    #[serde(default)]
    raw_stacktraces: Vec<serde_json::Value>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

impl JsWireResponse {
    fn into_domain(
        self,
        origins: &[RawTraceOrigin],
    ) -> Result<BackendSymbolicationResult, SymbolicationBackendError> {
        if self.stacktraces.len() != origins.len()
            || (!self.raw_stacktraces.is_empty() && self.raw_stacktraces.len() != origins.len())
        {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let derived = self
            .stacktraces
            .into_iter()
            .zip(origins.iter().copied())
            .map(|(trace, origin)| SymbolicatedStacktrace {
                origin,
                frames: trace
                    .frames
                    .into_iter()
                    .enumerate()
                    .map(|(original_index, frame)| SymbolicatedFrame {
                        original_index,
                        function: frame.function.map(Into::into),
                        filename: frame.filename.map(Into::into),
                        module: frame.module.map(Into::into),
                        line: frame.lineno,
                        column: frame.colno,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let has_frames = derived.iter().any(|trace| !trace.frames.is_empty());
        let has_errors = !self.errors.is_empty();
        let status = match (has_frames, has_errors) {
            (true, true) => BackendSymbolicationStatus::Partial,
            (true, false) => BackendSymbolicationStatus::Complete,
            (false, _) => BackendSymbolicationStatus::Missing,
        };
        Ok(BackendSymbolicationResult {
            status,
            derived,
            missing_debug_ids: Vec::new(),
            diagnostics: has_errors
                .then_some(SymbolicationDiagnosticCode::BackendPartial)
                .into_iter()
                .collect(),
        })
    }
}

#[derive(Deserialize)]
struct JsResponseTrace {
    #[serde(default)]
    frames: Vec<JsResponseFrame>,
}

#[derive(Deserialize)]
struct JsResponseFrame {
    function: Option<String>,
    filename: Option<String>,
    module: Option<String>,
    lineno: Option<u64>,
    colno: Option<u64>,
}

#[derive(Deserialize)]
struct WireResponse {
    status: String,
    #[serde(default)]
    stacktraces: Vec<WireSymbolicatedTrace>,
    #[serde(default)]
    missing_debug_ids: Vec<String>,
}

impl WireResponse {
    fn into_domain(
        self,
        origins: &[RawTraceOrigin],
    ) -> Result<BackendSymbolicationResult, SymbolicationBackendError> {
        if self.status == "pending" {
            return Err(SymbolicationBackendError::Timeout);
        }
        if self.status == "error" {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        if self.status != "complete" {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        if self.stacktraces.len() != origins.len() {
            return Err(SymbolicationBackendError::MalformedResponse);
        }
        let mut had_missing = false;
        let derived = self
            .stacktraces
            .into_iter()
            .zip(origins.iter().copied())
            .map(|(trace, origin)| trace.into_domain(origin, &mut had_missing))
            .collect::<Result<Vec<_>, _>>()?;
        let has_symbols = derived.iter().any(|trace| !trace.frames.is_empty());
        let status = if had_missing && has_symbols {
            BackendSymbolicationStatus::Partial
        } else if had_missing || !has_symbols {
            BackendSymbolicationStatus::Missing
        } else {
            BackendSymbolicationStatus::Complete
        };
        Ok(BackendSymbolicationResult {
            status,
            derived,
            missing_debug_ids: self
                .missing_debug_ids
                .into_iter()
                .map(String::into_boxed_str)
                .collect(),
            diagnostics: if status == BackendSymbolicationStatus::Partial {
                vec![SymbolicationDiagnosticCode::BackendPartial]
            } else {
                Vec::new()
            },
        })
    }
}

#[derive(Deserialize)]
struct WireSymbolicatedTrace {
    #[serde(default)]
    origin: Option<String>,
    frames: Vec<WireSymbolicatedFrame>,
}

impl WireSymbolicatedTrace {
    fn into_domain(
        self,
        expected_origin: RawTraceOrigin,
        had_missing: &mut bool,
    ) -> Result<SymbolicatedStacktrace, SymbolicationBackendError> {
        if let Some(origin) = &self.origin {
            if parse_origin(origin)? != expected_origin {
                return Err(SymbolicationBackendError::MalformedResponse);
            }
        }
        Ok(SymbolicatedStacktrace {
            origin: expected_origin,
            frames: self
                .frames
                .into_iter()
                .filter_map(|frame| {
                    let symbolicated =
                        frame.status.as_deref() == Some("symbolicated") || frame.function.is_some();
                    if !symbolicated {
                        *had_missing = true;
                        return None;
                    }
                    Some(SymbolicatedFrame {
                        original_index: frame.original_index,
                        function: frame.function.map(String::into_boxed_str),
                        filename: frame.filename.map(String::into_boxed_str),
                        module: frame.module.map(String::into_boxed_str),
                        line: frame.line,
                        column: frame.column,
                    })
                })
                .collect(),
        })
    }
}

#[derive(Deserialize)]
struct WireSymbolicatedFrame {
    #[serde(default)]
    status: Option<String>,
    original_index: usize,
    function: Option<String>,
    filename: Option<String>,
    module: Option<String>,
    #[serde(rename = "lineno")]
    line: Option<u64>,
    #[serde(rename = "colno")]
    column: Option<u64>,
}

fn parse_origin(value: &str) -> Result<RawTraceOrigin, SymbolicationBackendError> {
    if value == "event" {
        return Ok(RawTraceOrigin::Event);
    }
    let (kind, index) = value
        .split_once(':')
        .ok_or(SymbolicationBackendError::MalformedResponse)?;
    let index = index
        .parse()
        .map_err(|_| SymbolicationBackendError::MalformedResponse)?;
    match kind {
        "exception" => Ok(RawTraceOrigin::Exception { index }),
        "exception_raw" => Ok(RawTraceOrigin::ExceptionRaw { index }),
        _ => Err(SymbolicationBackendError::MalformedResponse),
    }
}

#[derive(Clone, Copy)]
enum PrivateSourceKind {
    DebugFile,
    ArtifactBundle,
}

impl PrivateSourceKind {
    const fn domain(self) -> &'static [u8] {
        match self {
            Self::DebugFile => b"metric/symbolicator-debug-source/v1",
            Self::ArtifactBundle => b"metric/symbolicator-artifact-source/v1",
        }
    }
}

fn token_with(key: &[u8], project_id: ProjectId, kind: PrivateSourceKind) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts arbitrary key sizes");
    mac.update(kind.domain());
    mac.update(&project_id.get().to_be_bytes());
    format!(
        "sym1.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

fn verify_with(key: &[u8], project_id: ProjectId, token: &str, kind: PrivateSourceKind) -> bool {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let Some(encoded) = token.strip_prefix("sym1.") else {
        return false;
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts arbitrary key sizes");
    mac.update(kind.domain());
    mac.update(&project_id.get().to_be_bytes());
    mac.verify_slice(&bytes).is_ok()
}

fn classify_reqwest(error: reqwest::Error) -> SymbolicationBackendError {
    if error.is_timeout() {
        SymbolicationBackendError::Timeout
    } else {
        SymbolicationBackendError::Unavailable
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{
        event::NormalizedFrame,
        symbolication::{RawStacktrace, SymbolicationKind, SymbolicationModule},
    };
    use std::collections::BTreeMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn private_source_token_is_project_scoped_and_rotatable() {
        let signer = PrivateSourceSigner::new(vec![1; 32], Some(vec![2; 32])).unwrap();
        let project = ProjectId::new(7).unwrap();
        let token = signer.token(project);
        assert!(signer.verify(project, &token));
        assert!(!signer.verify(ProjectId::new(8).unwrap(), &token));
        assert!(!signer.verify(project, "sym1.invalid"));
        assert!(!signer.verify_artifact(project, &token));
        assert_ne!(token, signer.artifact_token(project));
    }

    #[tokio::test]
    async fn pinned_javascript_contract_uses_artifact_revision_and_maps_frames() {
        let pinned: serde_json::Value = serde_json::from_str(include_str!(
            "../../../sdk-tests/symbolicator/26.6.0-javascript-contract.json"
        ))
        .unwrap();
        let response_body = serde_json::to_string(&pinned["response"]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0_u8; 32 * 1024];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.starts_with("POST /symbolicate-js?scope=project-7 "));
            assert!(request.contains("artifact-lookup/?revision=43"));
            assert!(request.contains("\"stacktraces\""));
            assert!(request.contains("\"scraping\":{\"enabled\":false}"));
            let body = response_body;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
        });
        let adapter = ExternalSymbolicator::new(
            ExternalSymbolicatorConfig {
                endpoint: Url::parse(&format!("http://{address}/symbolicate")).unwrap(),
                callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                request_timeout: Duration::from_secs(2),
                maximum_concurrency: 1,
                circuit_failure_threshold: 2,
                circuit_cooldown: Duration::from_secs(30),
                maximum_response_bytes: 4096,
            },
            PrivateSourceSigner::new(vec![1; 32], None).unwrap(),
        )
        .unwrap();
        let frame = NormalizedFrame {
            filename: Some("app.min.js".into()),
            absolute_path: Some("https://example.invalid/static/app.min.js".into()),
            function: Some("fail".into()),
            module: None,
            package: None,
            instruction_address: None,
            symbol_address: None,
            line: Some(1),
            column: Some(50),
            in_app: Some(true),
            context_line: None,
            pre_context: Vec::new(),
            post_context: Vec::new(),
            variables: BTreeMap::new(),
            unknown: BTreeMap::new(),
        };
        let output = adapter
            .symbolicate(SymbolicationRequest {
                project_id: ProjectId::new(7).unwrap(),
                debug_file_revision: 42,
                artifact_revision: 43,
                kind: SymbolicationKind::JavaScript,
                traces: vec![RawStacktrace {
                    origin: RawTraceOrigin::Event,
                    frames: vec![frame],
                }],
                modules: vec![SymbolicationModule {
                    kind: Some("sourcemap".into()),
                    debug_id: Some("67e9247c-814e-392b-a027-dbde6748fcbf".into()),
                    code_id: None,
                    code_file: Some("https://example.invalid/static/app.min.js".into()),
                    image_address: None,
                    image_size: None,
                }],
                release: Some("metric-phase18@1.0.0".into()),
                dist: Some("windows".into()),
            })
            .await
            .unwrap();
        assert_eq!(output.status, BackendSymbolicationStatus::Complete);
        assert_eq!(
            output.derived[0].frames[0].function.as_deref(),
            Some("fail")
        );
        assert_eq!(output.derived[0].frames[0].line, Some(6));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fake_external_contract_carries_revision_and_circuit_opens() {
        let pinned: serde_json::Value = serde_json::from_str(include_str!(
            "../../../sdk-tests/symbolicator/26.6.0-native-contract.json"
        ))
        .unwrap();
        assert_eq!(pinned["image"], "ghcr.io/getsentry/symbolicator:26.6.0");
        let response_body = serde_json::to_string(&pinned["response"]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0_u8; 16 * 1024];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.contains("revision=42"));
            assert!(request.contains("private-project-7"));
            let body = response_body;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
        });
        let endpoint = Url::parse(&format!("http://{address}/symbolicate")).unwrap();
        let signer = PrivateSourceSigner::new(vec![1; 32], None).unwrap();
        let adapter = ExternalSymbolicator::new(
            ExternalSymbolicatorConfig {
                endpoint,
                callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                request_timeout: Duration::from_secs(2),
                maximum_concurrency: 1,
                circuit_failure_threshold: 1,
                circuit_cooldown: Duration::from_secs(30),
                maximum_response_bytes: 4096,
            },
            signer,
        )
        .unwrap();
        let request = SymbolicationRequest {
            project_id: ProjectId::new(7).unwrap(),
            debug_file_revision: 42,
            artifact_revision: 0,
            kind: SymbolicationKind::Native,
            traces: Vec::new(),
            modules: Vec::new(),
            release: None,
            dist: None,
        };
        let output = adapter.symbolicate(request).await.unwrap();
        assert_eq!(output.status, BackendSymbolicationStatus::Missing);
        server.await.unwrap();

        let unavailable = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unavailable_address = unavailable.local_addr().unwrap();
        let failure_server = tokio::spawn(async move {
            let (mut stream, _) = unavailable.accept().await.unwrap();
            let mut bytes = [0_u8; 4096];
            let _ = stream.read(&mut bytes).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let failing = ExternalSymbolicator::new(
            ExternalSymbolicatorConfig {
                endpoint: Url::parse(&format!("http://{unavailable_address}/symbolicate")).unwrap(),
                callback_base_url: Url::parse("http://127.0.0.1:4001/").unwrap(),
                request_timeout: Duration::from_secs(2),
                maximum_concurrency: 1,
                circuit_failure_threshold: 1,
                circuit_cooldown: Duration::from_secs(30),
                maximum_response_bytes: 4096,
            },
            PrivateSourceSigner::new(vec![1; 32], None).unwrap(),
        )
        .unwrap();
        let request = SymbolicationRequest {
            project_id: ProjectId::new(7).unwrap(),
            debug_file_revision: 1,
            artifact_revision: 0,
            kind: SymbolicationKind::Native,
            traces: Vec::new(),
            modules: Vec::new(),
            release: None,
            dist: None,
        };
        assert_eq!(
            failing.symbolicate(request.clone()).await,
            Err(SymbolicationBackendError::Unavailable)
        );
        failure_server.await.unwrap();
        assert_eq!(
            failing.symbolicate(request).await,
            Err(SymbolicationBackendError::Unavailable)
        );
    }
}
