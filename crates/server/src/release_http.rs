//! Accepted `sentry-cli` Release and Deploy compatibility subset.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{post, put},
};
use metric_application::{
    auth::IdentityService,
    releases::{CreateDeployRequest, ReleaseError, ReleaseService},
};
use metric_domain::{
    Timestamp,
    auth::{AuthContext, PlainSecret},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const MAX_BODY_PROJECTS: usize = 256;

#[derive(Clone)]
struct ReleaseHttpState {
    identity: Arc<IdentityService>,
    releases: Arc<ReleaseService>,
}

#[derive(Debug)]
enum ReleaseHttpError {
    Invalid,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Unavailable,
}

impl IntoResponse for ReleaseHttpError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid_credentials"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict => (StatusCode::CONFLICT, "conflict"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        };
        (
            status,
            Json(json!({ "detail": code, "error": { "code": code } })),
        )
            .into_response()
    }
}

pub fn router(
    identity: Option<Arc<IdentityService>>,
    releases: Option<Arc<ReleaseService>>,
) -> Router {
    let (Some(identity), Some(releases)) = (identity, releases) else {
        return Router::new();
    };
    Router::new()
        .route(
            "/api/0/organizations/{organization_slug}/releases/",
            post(create_release),
        )
        .route(
            "/api/0/organizations/{organization_slug}/releases/{version}/",
            put(finalize_release),
        )
        .route(
            "/api/0/organizations/{organization_slug}/releases/{version}/deploys/",
            post(create_deploy),
        )
        .with_state(ReleaseHttpState { identity, releases })
}

#[derive(Debug, Deserialize)]
struct CompatibleCreateRelease {
    version: String,
    #[serde(default)]
    projects: Vec<String>,
    url: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    #[serde(rename = "dateReleased")]
    date_released: Option<String>,
}

async fn create_release(
    State(state): State<ReleaseHttpState>,
    Path(organization_slug): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CompatibleCreateRelease>,
) -> Result<Json<Value>, ReleaseHttpError> {
    let context = authenticate(&state, &headers).await?;
    if body.projects.is_empty() || body.projects.len() > MAX_BODY_PROJECTS {
        return Err(ReleaseHttpError::Invalid);
    }
    let (_, projects) = state
        .releases
        .resolve_cli_projects(
            &context,
            &organization_slug,
            body.projects
                .iter()
                .map(|value| value.clone().into_boxed_str())
                .collect(),
        )
        .await
        .map_err(map_release)?;
    let release = state
        .releases
        .create(
            &context,
            projects,
            body.version.into_boxed_str(),
            body.url.map(String::into_boxed_str),
            body.ref_.map(String::into_boxed_str),
            Vec::new(),
        )
        .await
        .map_err(map_release)?;
    let release = if let Some(value) = body.date_released {
        state
            .releases
            .finalize(&context, release.id, Some(parse_timestamp(&value)?))
            .await
            .map_err(map_release)?
    } else {
        release
    };
    compatible_release(&release)
}

#[derive(Debug, Deserialize)]
struct CompatibleFinalizeRelease {
    url: Option<String>,
    #[serde(rename = "dateReleased")]
    date_released: Option<String>,
}

async fn finalize_release(
    State(state): State<ReleaseHttpState>,
    Path((_organization_slug, version)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<CompatibleFinalizeRelease>,
) -> Result<Json<Value>, ReleaseHttpError> {
    let context = authenticate(&state, &headers).await?;
    let release = state
        .releases
        .load_version(&context, &version)
        .await
        .map_err(map_release)?;
    if body.url.is_some() && body.url.as_deref() != release.url.as_deref() {
        return Err(ReleaseHttpError::Conflict);
    }
    let released_at = body
        .date_released
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let release = state
        .releases
        .finalize(&context, release.id, released_at)
        .await
        .map_err(map_release)?;
    compatible_release(&release)
}

#[derive(Debug, Deserialize)]
struct CompatibleDeploy {
    environment: String,
    name: Option<String>,
    url: Option<String>,
    #[serde(rename = "dateStarted")]
    date_started: Option<String>,
    #[serde(rename = "dateFinished")]
    date_finished: Option<String>,
}

async fn create_deploy(
    State(state): State<ReleaseHttpState>,
    Path((_organization_slug, version)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<CompatibleDeploy>,
) -> Result<Json<Value>, ReleaseHttpError> {
    let context = authenticate(&state, &headers).await?;
    let release = state
        .releases
        .load_version(&context, &version)
        .await
        .map_err(map_release)?;
    let started_at = body
        .date_started
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let finished_at = body
        .date_finished
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let operation_id = compatible_operation_id(
        &release.version,
        &body.environment,
        body.name.as_deref(),
        body.url.as_deref(),
        started_at,
        finished_at,
    );
    let deploy = state
        .releases
        .create_deploy(
            &context,
            release.id,
            release.project_ids,
            CreateDeployRequest {
                operation_id,
                environment: body.environment.into_boxed_str(),
                name: body.name.map(String::into_boxed_str),
                url: body.url.map(String::into_boxed_str),
                started_at,
                finished_at,
            },
        )
        .await
        .map_err(map_release)?;
    Ok(Json(json!({
        "id": deploy.id.to_string(),
        "environment": deploy.environment,
        "name": deploy.name,
        "url": deploy.url,
        "dateStarted": timestamp_string(deploy.started_at)?,
        "dateFinished": deploy.finished_at.map(timestamp_string).transpose()?,
    })))
}

async fn authenticate(
    state: &ReleaseHttpState,
    headers: &HeaderMap,
) -> Result<AuthContext, ReleaseHttpError> {
    let token = bearer(headers).ok_or(ReleaseHttpError::Unauthorized)?;
    state
        .identity
        .authenticate_api_token(&plain_secret(token)?)
        .await
        .map_err(|_| ReleaseHttpError::Unauthorized)
}

fn plain_secret(value: &str) -> Result<PlainSecret, ReleaseHttpError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ReleaseHttpError::Unauthorized);
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| ReleaseHttpError::Unauthorized)?;
    Ok(PlainSecret::new(bytes))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)?
                .to_str()
                .ok()?
                .strip_prefix("bearer ")
        })
}

fn compatible_release(
    release: &metric_domain::releases::ReleaseRecord,
) -> Result<Json<Value>, ReleaseHttpError> {
    Ok(Json(json!({
        "version": release.version,
        "ref": release.reference,
        "url": release.url,
        "dateCreated": timestamp_string(release.created_at)?,
        "dateReleased": release.released_at.map(timestamp_string).transpose()?,
    })))
}

fn compatible_operation_id(
    release: &str,
    environment: &str,
    name: Option<&str>,
    url: Option<&str>,
    started_at: Option<Timestamp>,
    finished_at: Option<Timestamp>,
) -> [u8; 16] {
    let mut digest = Sha256::new();
    for value in [release, environment, name.unwrap_or(""), url.unwrap_or("")] {
        digest.update(value.len().to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.update(
        started_at
            .map_or(i64::MIN, Timestamp::unix_millis)
            .to_be_bytes(),
    );
    digest.update(
        finished_at
            .map_or(i64::MIN, Timestamp::unix_millis)
            .to_be_bytes(),
    );
    digest.finalize()[..16]
        .try_into()
        .expect("SHA-256 has at least 16 bytes")
}

fn parse_timestamp(value: &str) -> Result<Timestamp, ReleaseHttpError> {
    let value = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ReleaseHttpError::Invalid)?;
    let milliseconds = value.unix_timestamp_nanos() / 1_000_000;
    Timestamp::from_unix_millis(i64::try_from(milliseconds).map_err(|_| ReleaseHttpError::Invalid)?)
        .map_err(|_| ReleaseHttpError::Invalid)
}

fn timestamp_string(value: Timestamp) -> Result<String, ReleaseHttpError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value.unix_millis()) * 1_000_000)
        .map_err(|_| ReleaseHttpError::Invalid)?
        .format(&Rfc3339)
        .map_err(|_| ReleaseHttpError::Invalid)
}

fn map_release(error: ReleaseError) -> ReleaseHttpError {
    match error {
        ReleaseError::InvalidRequest => ReleaseHttpError::Invalid,
        ReleaseError::Forbidden => ReleaseHttpError::Forbidden,
        ReleaseError::NotFound => ReleaseHttpError::NotFound,
        ReleaseError::Conflict => ReleaseHttpError::Conflict,
        ReleaseError::Unavailable => ReleaseHttpError::Unavailable,
    }
}
