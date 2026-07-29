//! Static delivery for the Phase 13 Web client.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::Response,
};
use tower_http::services::{ServeDir, ServeFile};

const WEB_ROOT_ENV: &str = "METRIC_WEB_DIR";

/// Serves only known Web routes. Unknown API paths keep their normal 404 response
/// instead of being rewritten to the SPA entry point.
pub fn router() -> Router {
    let root = std::env::var_os(WEB_ROOT_ENV).unwrap_or_else(|| OsString::from("web/dist"));
    router_with_root(PathBuf::from(root))
}

/// Serves the Web client from an explicit asset root.
///
/// Production normally selects the same root through `METRIC_WEB_DIR`.
pub fn router_with_root(root: impl AsRef<Path>) -> Router {
    let root = root.as_ref();
    let index = ServeFile::new(root.join("index.html"));
    Router::new()
        .route_service("/", index.clone())
        .route_service("/dashboard", index.clone())
        .route_service("/dashboards", index.clone())
        .route_service("/issues", index.clone())
        .route_service("/issues/{issue_id}", index.clone())
        .route_service("/events/{event_id}", index.clone())
        .route_service("/logs", index.clone())
        .route_service("/logs/{log_id}", index.clone())
        .route_service("/traces", index.clone())
        .route_service("/traces/{trace_id}", index.clone())
        .route_service("/performance", index.clone())
        .route_service("/explore", index.clone())
        .route_service("/metrics", index.clone())
        .route_service("/alerts", index.clone())
        .route_service("/monitors", index.clone())
        .route_service("/feedback", index.clone())
        .route_service("/feedback/{feedback_id}", index.clone())
        .route_service("/replays", index.clone())
        .route_service("/replays/{replay_id}", index.clone())
        .route_service("/releases", index.clone())
        .route_service("/releases/{release_id}", index.clone())
        .route_service("/organization", index.clone())
        .route_service("/account/tokens", index.clone())
        .route_service("/auth/setup", index.clone())
        .route_service("/projects/new", index.clone())
        .route_service("/project/setup", index.clone())
        .route_service("/project/settings", index.clone())
        .route_service("/settings", index.clone())
        .route_service("/settings/project", index.clone())
        .route_service("/settings/notifications", index.clone())
        .route_service("/settings/organization", index.clone())
        .route_service("/settings/system", index.clone())
        .route_service("/system", index)
        .route_service("/favicon.svg", ServeFile::new(root.join("favicon.svg")))
        .nest_service("/assets", ServeDir::new(root.join("assets")))
        .nest_service("/fonts", ServeDir::new(root.join("fonts")))
        .layer(middleware::from_fn(security_headers))
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
             script-src 'self'; style-src 'self'; \
             style-src-elem 'self' 'unsafe-inline'; style-src-attr 'unsafe-inline'; \
             object-src 'none'; \
             base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_known_spa_routes_with_security_headers() {
        let root = std::env::temp_dir().join(format!(
            "metric-web-http-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "<main>Metric Web</main>").unwrap();

        for path in [
            "/dashboard",
            "/dashboards",
            "/issues/001122",
            "/logs",
            "/logs/001122",
            "/traces",
            "/traces/001122",
            "/performance",
            "/explore",
            "/metrics",
            "/alerts",
            "/monitors",
            "/feedback",
            "/feedback/001122",
            "/replays",
            "/replays/001122",
            "/releases",
            "/releases/001122",
            "/organization",
            "/account/tokens",
            "/auth/setup",
            "/projects/new",
            "/settings",
            "/settings/project",
            "/settings/notifications",
            "/settings/organization",
            "/settings/system",
        ] {
            let response = router_with_root(&root)
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response
                    .headers()
                    .get(header::X_CONTENT_TYPE_OPTIONS)
                    .unwrap(),
                "nosniff"
            );
            let content_security_policy = response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap();
            assert!(content_security_policy.contains("script-src 'self'"));
            assert!(
                content_security_policy.contains(
                    "style-src-elem 'self' 'unsafe-inline'; style-src-attr 'unsafe-inline'"
                )
            );
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(body, "<main>Metric Web</main>");
        }

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn serves_favicon_and_self_hosted_fonts() {
        let root = std::env::temp_dir().join(format!(
            "metric-web-fonts-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join("fonts")).unwrap();
        std::fs::write(root.join("index.html"), "<main>Metric Web</main>").unwrap();
        std::fs::write(root.join("favicon.svg"), "<svg/>").unwrap();
        std::fs::write(
            root.join("fonts")
                .join("jetbrains-mono-latin-wght-normal.woff2"),
            [0u8; 8],
        )
        .unwrap();

        for path in [
            "/favicon.svg",
            "/fonts/jetbrains-mono-latin-wght-normal.woff2",
        ] {
            let response = router_with_root(&root)
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn does_not_rewrite_unknown_api_routes() {
        let response = router_with_root(std::env::temp_dir().join("metric-web-missing"))
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
