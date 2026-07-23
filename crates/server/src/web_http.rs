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

const WEB_ROOT_ENV: &str = "FAULTKEEP_WEB_DIR";

/// Serves only known Web routes. Unknown API paths keep their normal 404 response
/// instead of being rewritten to the SPA entry point.
pub fn router() -> Router {
    let root = std::env::var_os(WEB_ROOT_ENV).unwrap_or_else(|| OsString::from("web/dist"));
    router_from(PathBuf::from(root))
}

fn router_from(root: impl AsRef<Path>) -> Router {
    let root = root.as_ref();
    let index = ServeFile::new(root.join("index.html"));
    Router::new()
        .route_service("/", index.clone())
        .route_service("/issues", index.clone())
        .route_service("/issues/{issue_id}", index.clone())
        .route_service("/events/{event_id}", index.clone())
        .route_service("/project/setup", index.clone())
        .route_service("/project/settings", index.clone())
        .route_service("/system", index)
        .nest_service("/assets", ServeDir::new(root.join("assets")))
        .layer(middleware::from_fn(security_headers))
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
             script-src 'self'; style-src 'self'; object-src 'none'; \
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
            "faultkeep-web-http-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "<main>Faultkeep Web</main>").unwrap();

        let response = router_from(&root)
            .oneshot(
                Request::builder()
                    .uri("/issues/001122")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .unwrap(),
            "nosniff"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body, "<main>Faultkeep Web</main>");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn does_not_rewrite_unknown_api_routes() {
        let response = router_from(std::env::temp_dir().join("faultkeep-web-missing"))
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
