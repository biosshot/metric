use std::{
    io,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{MatchedPath, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use metric_application::{
    observability::{Metric, Metrics, Outcome, RequestId},
    shutdown::ShutdownSignal,
};
use metric_ports::PortFuture;
use serde::Serialize;
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tower::ServiceExt;
use tracing::{Instrument, info_span};

#[derive(Clone)]
struct HttpState {
    shutdown: ShutdownSignal,
    metrics: Metrics,
    readiness: Readiness,
}

pub trait DependencyReadiness: Send + Sync + 'static {
    fn check(&self) -> PortFuture<'_, bool>;
}

#[derive(Clone)]
pub struct Readiness {
    composed: bool,
    tasks: Arc<[tokio::task::AbortHandle]>,
    dependency: Option<Arc<dyn DependencyReadiness>>,
}

impl Readiness {
    #[must_use]
    pub fn new(
        composed: bool,
        tasks: Vec<tokio::task::AbortHandle>,
        dependency: Option<Arc<dyn DependencyReadiness>>,
    ) -> Self {
        Self {
            composed,
            tasks: tasks.into(),
            dependency,
        }
    }

    #[must_use]
    pub fn fixed(ready: bool) -> Self {
        Self::new(ready, Vec::new(), None)
    }

    pub async fn is_ready(&self) -> bool {
        self.composed
            && self.tasks.iter().all(|task| !task.is_finished())
            && match &self.dependency {
                Some(dependency) => dependency.check().await,
                None => true,
            }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPhase {
    Starting,
    InspectingSchema,
    WaitingForMigration,
    Migrating,
    StartingServices,
}

impl StartupPhase {
    fn code(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::InspectingSchema => "inspecting_schema",
            Self::WaitingForMigration => "waiting_for_migration",
            Self::Migrating => "migrating",
            Self::StartingServices => "starting_services",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StartupProgress {
    pub phase: StartupPhase,
    pub completed_steps: u32,
    pub total_steps: u32,
    pub processed_records: u64,
    pub warnings: u64,
}

impl Default for StartupProgress {
    fn default() -> Self {
        Self {
            phase: StartupPhase::Starting,
            completed_steps: 0,
            total_steps: 0,
            processed_records: 0,
            warnings: 0,
        }
    }
}

#[derive(Clone)]
pub struct StartupGate {
    inner: Arc<RwLock<StartupGateState>>,
}

struct StartupGateState {
    progress: StartupProgress,
    application: Option<(Router, Readiness)>,
}

impl StartupGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(StartupGateState {
                progress: StartupProgress::default(),
                application: None,
            })),
        }
    }

    pub fn report(&self, progress: StartupProgress) {
        let mut state = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if state.application.is_none() {
            state.progress = progress;
        }
    }

    pub fn activate(&self, application: Router, readiness: Readiness) {
        let mut state = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        state.application = Some((application, readiness));
    }

    fn is_active(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .application
            .is_some()
    }

    fn snapshot(&self) -> StartupGateSnapshot {
        let state = self.inner.read().unwrap_or_else(|error| error.into_inner());
        StartupGateSnapshot {
            progress: state.progress,
            application: state.application.clone(),
        }
    }
}

impl Default for StartupGate {
    fn default() -> Self {
        Self::new()
    }
}

struct StartupGateSnapshot {
    progress: StartupProgress,
    application: Option<(Router, Readiness)>,
}

#[derive(Clone)]
struct StartupHttpState {
    shutdown: ShutdownSignal,
    metrics: Metrics,
    gate: StartupGate,
}

#[derive(Serialize)]
struct ProbeResponse {
    status: &'static str,
}

pub fn router(shutdown: ShutdownSignal, metrics: Metrics, application_routes: Router) -> Router {
    router_with_readiness(shutdown, metrics, application_routes, true)
}

pub fn router_with_readiness(
    shutdown: ShutdownSignal,
    metrics: Metrics,
    application_routes: Router,
    required_ready: bool,
) -> Router {
    router_with_probe(
        shutdown,
        metrics,
        application_routes,
        Readiness::fixed(required_ready),
    )
}

pub fn router_with_probe(
    shutdown: ShutdownSignal,
    metrics: Metrics,
    application_routes: Router,
    readiness: Readiness,
) -> Router {
    let state = HttpState {
        shutdown,
        metrics,
        readiness,
    };
    let live_routes = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .with_state(state.clone());
    live_routes
        .merge(application_routes)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_context,
        ))
}

pub fn startup_router(shutdown: ShutdownSignal, metrics: Metrics, gate: StartupGate) -> Router {
    let startup_state = StartupHttpState {
        shutdown: shutdown.clone(),
        metrics,
        gate,
    };
    Router::new()
        .route("/live", get(startup_live))
        .route("/ready", get(startup_ready))
        .fallback(startup_request)
        .with_state(startup_state.clone())
        .layer(middleware::from_fn_with_state(
            startup_state,
            startup_request_context,
        ))
}

async fn startup_request_context(
    State(state): State<StartupHttpState>,
    request: Request,
    next: Next,
) -> Response {
    let probe = matches!(request.uri().path(), "/live" | "/ready");
    if state.gate.is_active() && !probe {
        return next.run(request).await;
    }
    let route_override = (!probe).then_some("/startup-maintenance");
    observe_request(state.metrics, request, next, route_override).await
}

async fn startup_live(State(state): State<StartupHttpState>) -> Response {
    if state.shutdown.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProbeResponse {
                status: "shutting_down",
            }),
        )
            .into_response();
    }
    (StatusCode::OK, Json(ProbeResponse { status: "ok" })).into_response()
}

async fn startup_ready(State(state): State<StartupHttpState>) -> Response {
    if state.shutdown.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProbeResponse {
                status: "shutting_down",
            }),
        )
            .into_response();
    }
    let snapshot = state.gate.snapshot();
    let Some((_, readiness)) = snapshot.application else {
        return startup_json_response(snapshot.progress);
    };
    if !readiness.is_ready().await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProbeResponse {
                status: "required_dependency_unavailable",
            }),
        )
            .into_response();
    }
    (StatusCode::OK, Json(ProbeResponse { status: "ready" })).into_response()
}

async fn startup_request(State(state): State<StartupHttpState>, request: Request) -> Response {
    let snapshot = state.gate.snapshot();
    if let Some((application, _)) = snapshot.application {
        return match application.oneshot(request).await {
            Ok(response) => response,
            Err(error) => match error {},
        };
    }

    let wants_html = matches!(request.method(), &Method::GET | &Method::HEAD)
        && request
            .headers()
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/html"));
    if wants_html {
        let russian = request
            .headers()
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(prefers_russian);
        let head = request.method() == Method::HEAD;
        startup_html_response(snapshot.progress, russian, head)
    } else {
        startup_json_response(snapshot.progress)
    }
}

#[derive(Serialize)]
struct StartupResponse {
    status: &'static str,
    completed_steps: u32,
    total_steps: u32,
    remaining_steps: u32,
    processed_records: u64,
    warnings: u64,
}

fn startup_json_response(progress: StartupProgress) -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(StartupResponse {
            status: progress.phase.code(),
            completed_steps: progress.completed_steps,
            total_steps: progress.total_steps,
            remaining_steps: progress
                .total_steps
                .saturating_sub(progress.completed_steps),
            processed_records: progress.processed_records,
            warnings: progress.warnings,
        }),
    )
        .into_response();
    maintenance_headers(&mut response);
    response
}

fn startup_html_response(progress: StartupProgress, russian: bool, head: bool) -> Response {
    let completed = progress.completed_steps.min(progress.total_steps);
    let remaining = progress.total_steps.saturating_sub(completed);
    let percent = if progress.total_steps == 0 {
        0
    } else {
        completed.saturating_mul(100) / progress.total_steps
    };
    let (language, title, heading, phase_label, remaining_label, processed_label, warning_label) =
        if russian {
            (
                "ru",
                "Metric обновляется",
                "Metric обновляется",
                russian_phase(progress.phase),
                "Осталось шагов",
                "Обработано документов",
                "Предупреждений",
            )
        } else {
            (
                "en",
                "Metric is updating",
                "Metric is updating",
                english_phase(progress.phase),
                "Steps remaining",
                "Documents processed",
                "Warnings",
            )
        };
    let step_text = if progress.total_steps == 0 {
        String::new()
    } else if russian {
        format!(
            "<p class=\"detail\">Шаг {} из {} · {remaining_label}: {remaining}</p>",
            completed.saturating_add(1).min(progress.total_steps),
            progress.total_steps,
        )
    } else {
        format!(
            "<p class=\"detail\">Step {} of {} · {remaining_label}: {remaining}</p>",
            completed.saturating_add(1).min(progress.total_steps),
            progress.total_steps,
        )
    };
    let processed_text = if progress.processed_records == 0 {
        String::new()
    } else {
        format!(
            "<p class=\"detail\">{processed_label}: {}</p>",
            progress.processed_records
        )
    };
    let warning_text = if progress.warnings == 0 {
        String::new()
    } else {
        format!(
            "<p class=\"detail warning\">{warning_label}: {}</p>",
            progress.warnings
        )
    };
    let body = if head {
        String::new()
    } else {
        format!(
            "<!doctype html><html lang=\"{language}\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
             <meta http-equiv=\"refresh\" content=\"2\"><title>{title}</title><style>\
             :root{{color-scheme:dark;font-family:'JetBrains Mono',ui-monospace,monospace}}\
             *{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:grid;place-items:center;\
             background:#0a0a0a;color:#f2f2f2;padding:24px}}main{{width:min(560px,100%);\
             border:1px solid #303030;background:#101010;padding:32px}}h1{{font-size:20px;\
             margin:0 0 12px}}.phase{{color:#a8a8a8;margin:0 0 24px}}.track{{height:12px;\
             border:1px solid #777;background:#171717;overflow:hidden}}.bar{{display:block;height:100%;\
             width:{percent}%;background:#f2f2f2}}.detail{{color:#a8a8a8;font-size:13px;margin:12px 0 0}}\
             .warning{{color:#c6aa78}}@media(max-width:520px){{main{{padding:24px 20px}}}}\
             @media(prefers-reduced-motion:reduce){{*{{scroll-behavior:auto!important}}}}</style></head>\
             <body><main><h1>{heading}</h1><p class=\"phase\" aria-live=\"polite\">{phase_label}</p>\
             <div class=\"track\" role=\"progressbar\" aria-valuemin=\"0\" aria-valuemax=\"100\"\
             aria-valuenow=\"{percent}\"><span class=\"bar\"></span></div>{step_text}{processed_text}\
             {warning_text}</main></body></html>"
        )
    };
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    maintenance_headers(&mut response);
    response
}

fn maintenance_headers(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

fn prefers_russian(value: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .filter_map(|part| part.split(';').next())
        .any(|language| {
            language.eq_ignore_ascii_case("ru")
                || language
                    .get(..3)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ru-"))
        })
}

fn english_phase(phase: StartupPhase) -> &'static str {
    match phase {
        StartupPhase::Starting => "Preparing startup",
        StartupPhase::InspectingSchema => "Checking the database schema",
        StartupPhase::WaitingForMigration => "Waiting for another migration process",
        StartupPhase::Migrating => "Updating the database schema",
        StartupPhase::StartingServices => "Starting application services",
    }
}

fn russian_phase(phase: StartupPhase) -> &'static str {
    match phase {
        StartupPhase::Starting => "Подготовка к запуску",
        StartupPhase::InspectingSchema => "Проверка схемы базы данных",
        StartupPhase::WaitingForMigration => "Ожидание другого процесса миграции",
        StartupPhase::Migrating => "Обновление схемы базы данных",
        StartupPhase::StartingServices => "Запуск сервисов приложения",
    }
}

async fn ready(State(state): State<HttpState>) -> Response {
    if state.shutdown.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProbeResponse {
                status: "shutting_down",
            }),
        )
            .into_response();
    }
    if !state.readiness.is_ready().await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProbeResponse {
                status: "required_dependency_unavailable",
            }),
        )
            .into_response();
    }
    (StatusCode::OK, Json(ProbeResponse { status: "ready" })).into_response()
}

async fn live(State(state): State<HttpState>) -> Response {
    if state.shutdown.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ProbeResponse {
                status: "shutting_down",
            }),
        )
            .into_response();
    }
    (StatusCode::OK, Json(ProbeResponse { status: "ok" })).into_response()
}

async fn request_context(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    observe_request(state.metrics, request, next, None).await
}

async fn observe_request(
    metrics: Metrics,
    mut request: Request,
    next: Next,
    route_override: Option<&'static str>,
) -> Response {
    let request_id = RequestId::from_bytes(*uuid::Uuid::new_v4().as_bytes());
    request.extensions_mut().insert(request_id);
    let method = request.method().clone();
    let route = route_override.map_or_else(
        || {
            request
                .extensions()
                .get::<MatchedPath>()
                .map_or_else(|| request.uri().path(), MatchedPath::as_str)
        },
        |route| route,
    );
    let span = info_span!(
        "http.request",
        request_id = %request_id,
        operation = "http.request",
        http.method = %method,
        http.route = route,
        http.status_code = tracing::field::Empty,
        http.status_class = tracing::field::Empty,
        outcome = tracing::field::Empty,
    );
    let response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    let (outcome, outcome_label) = response_outcome(status);
    span.record("http.status_code", status.as_u16());
    span.record(
        "http.status_class",
        tracing::field::display(format_args!("{}xx", status.as_u16() / 100)),
    );
    span.record("outcome", outcome_label);
    metrics.increment(Metric::HttpRequests, outcome);
    response
}

fn response_outcome(status: StatusCode) -> (Outcome, &'static str) {
    if status.is_server_error() {
        (Outcome::Error, "error")
    } else if status.is_client_error() {
        (Outcome::Rejected, "rejected")
    } else {
        (Outcome::Ok, "ok")
    }
}

pub async fn run(
    listener: TcpListener,
    shutdown: ShutdownSignal,
    shutdown_grace: Duration,
    app: Router,
) -> io::Result<()> {
    let server_shutdown = shutdown.clone();
    let mut server: JoinHandle<io::Result<()>> = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
        .await
    });

    tokio::select! {
        result = &mut server => join_server(result),
        () = shutdown.cancelled() => {
            match timeout(shutdown_grace, &mut server).await {
                Ok(result) => join_server(result),
                Err(_) => {
                    server.abort();
                    Err(io::Error::new(io::ErrorKind::TimedOut, "HTTP shutdown grace elapsed"))
                }
            }
        }
    }
}

fn join_server(result: Result<io::Result<()>, tokio::task::JoinError>) -> io::Result<()> {
    result.map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
        routing::get,
    };
    use metric_application::shutdown::ShutdownRoot;
    use tower::ServiceExt;

    #[tokio::test]
    async fn live_is_healthy_before_shutdown() {
        let root = ShutdownRoot::new();
        let response = router(root.signal(), Metrics, Router::new())
            .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn live_reflects_shutdown_fence() {
        let root = ShutdownRoot::new();
        let app = router(root.signal(), Metrics, Router::new());
        root.begin();
        let response = app
            .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn ready_requires_composed_durable_dependencies() {
        let root = ShutdownRoot::new();
        let app = router_with_readiness(root.signal(), Metrics, Router::new(), false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readiness_fails_when_a_required_worker_finishes() {
        let task = tokio::spawn(async {});
        let handle = task.abort_handle();
        task.await.unwrap();
        assert!(!Readiness::new(true, vec![handle], None).is_ready().await);
    }

    #[tokio::test]
    async fn startup_gate_keeps_application_routes_closed_until_activation() {
        let root = ShutdownRoot::new();
        let gate = StartupGate::new();
        gate.report(StartupProgress {
            phase: StartupPhase::Migrating,
            completed_steps: 2,
            total_steps: 5,
            processed_records: 120,
            warnings: 3,
        });
        let app = startup_router(root.signal(), Metrics, gate.clone());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(
            body,
            r#"{"status":"migrating","completed_steps":2,"total_steps":5,"remaining_steps":3,"processed_records":120,"warnings":3}"#
        );

        gate.activate(
            Router::new().route("/api/v1/projects", get(|| async { StatusCode::NO_CONTENT })),
            Readiness::fixed(true),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn startup_gate_serves_localized_accessible_progress_page() {
        let root = ShutdownRoot::new();
        let gate = StartupGate::new();
        gate.report(StartupProgress {
            phase: StartupPhase::Migrating,
            completed_steps: 1,
            total_steps: 4,
            processed_records: 25,
            warnings: 0,
        });
        let response = startup_router(root.signal(), Metrics, gate)
            .oneshot(
                Request::builder()
                    .uri("/issues")
                    .header(header::ACCEPT, "text/html")
                    .header(header::ACCEPT_LANGUAGE, "ru-RU,ru;q=0.9,en;q=0.8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("default-src 'none'")
        );
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("Metric обновляется"));
        assert!(body.contains("Шаг 2 из 4"));
        assert!(body.contains("Осталось шагов: 3"));
        assert!(body.contains("aria-valuenow=\"25\""));
        assert!(body.contains("Обработано документов: 25"));
    }

    #[tokio::test]
    async fn startup_readiness_remains_closed_until_runtime_activation() {
        let root = ShutdownRoot::new();
        let gate = StartupGate::new();
        let app = startup_router(root.signal(), Metrics, gate.clone());
        let live = app
            .clone()
            .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(live.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        gate.activate(Router::new(), Readiness::fixed(true));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn server_stops_within_grace_after_root_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = ShutdownRoot::new();
        let signal = root.signal();
        let app = router(signal.clone(), Metrics, Router::new());
        let server = tokio::spawn(run(listener, signal, Duration::from_secs(1), app));
        tokio::task::yield_now().await;
        root.begin();
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server did not stop")
            .expect("server task failed")
            .expect("server returned an error");
    }

    #[test]
    fn client_failures_are_not_reported_as_successful_http_traffic() {
        assert_eq!(response_outcome(StatusCode::OK).0, Outcome::Ok);
        assert_eq!(
            response_outcome(StatusCode::BAD_REQUEST).0,
            Outcome::Rejected
        );
        assert_eq!(
            response_outcome(StatusCode::TOO_MANY_REQUESTS).0,
            Outcome::Rejected
        );
        assert_eq!(
            response_outcome(StatusCode::SERVICE_UNAVAILABLE).0,
            Outcome::Error
        );
    }
}
