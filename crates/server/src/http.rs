use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{MatchedPath, Request, State},
    http::StatusCode,
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

async fn request_context(
    State(state): State<HttpState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = RequestId::from_bytes(*uuid::Uuid::new_v4().as_bytes());
    request.extensions_mut().insert(request_id);
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path(), MatchedPath::as_str)
        .to_owned();
    let span = info_span!(
        "http.request",
        request_id = %request_id,
        operation = "http.request",
        http.method = %method,
        http.route = %route,
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
    state.metrics.increment(Metric::HttpRequests, outcome);
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
    use axum::{body::Body, http::Request};
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
