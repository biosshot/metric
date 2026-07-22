use std::{io, time::Duration};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use faultkeep_application::{
    observability::{Metric, Metrics, Outcome, RequestId},
    shutdown::ShutdownSignal,
};
use serde::Serialize;
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tracing::{Instrument, info_span};

#[derive(Clone)]
struct HttpState {
    shutdown: ShutdownSignal,
    metrics: Metrics,
    required_ready: bool,
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
    let state = HttpState {
        shutdown,
        metrics,
        required_ready,
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
    if !state.required_ready {
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
    let span = info_span!(
        "http.request",
        request_id = %request_id,
        operation = "http.request",
    );
    let response = next.run(request).instrument(span).await;
    let outcome = if response.status().is_server_error() {
        Outcome::Error
    } else {
        Outcome::Ok
    };
    state.metrics.increment(Metric::HttpRequests, outcome);
    response
}

pub async fn run(
    listener: TcpListener,
    shutdown: ShutdownSignal,
    shutdown_grace: Duration,
    app: Router,
) -> io::Result<()> {
    let server_shutdown = shutdown.clone();
    let mut server: JoinHandle<io::Result<()>> = tokio::spawn(async move {
        axum::serve(listener, app)
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
    use faultkeep_application::shutdown::ShutdownRoot;
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
}
