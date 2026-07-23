//! Versioned native `/api/v1` HTTP adapter.

use std::{collections::BTreeMap, net::SocketAddr, num::NonZeroU32, sync::Arc};

use axum::{
    Json, Router,
    extract::{
        ConnectInfo, DefaultBodyLimit, Extension, Path, RawQuery, Request, State,
        rejection::JsonRejection,
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use faultkeep_application::{
    auth::{BootstrapRequest, CreateApiTokenRequest, IdentityService, LoginRequest, PasswordInput},
    native_api::{EventListRequest, NativeApiError, NativeApiService},
    observability::RequestId,
    projects::CreateProject,
    search::SearchError,
};
use faultkeep_domain::{
    BoundedId, DisplayName, DsnKey, EventId, IpScrubPolicy, ItemCapabilities, ProjectId,
    ProjectIngestLimits, ProjectKeyLabel, Slug, Timestamp,
    api::{
        EnvironmentView, EventView, IssueActivityKind, IssueActivityView, ProjectKeyView,
        ProjectPolicyUpdate, ProjectView, ReleaseView,
    },
    auth::{
        Actor, AuthContext, CredentialId, EmailAddress, OrganizationRole, Permission,
        PermissionSet, PlainSecret, RequestCorrelationId, SecretDigest, TokenName, UserDisplayName,
    },
    deletion::{ProjectDeletionOperationId, ProjectDeletionPhase, ProjectDeletionStatus},
    grouping::IssueId,
    issue::{ActorKind, ActorRef, IssueCommandAction, IssueSnapshot, IssueStatus},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const SESSION_COOKIE: &str = "faultkeep_session";
const ORGANIZATION_HEADER: &str = "x-faultkeep-organization-id";
const CSRF_HEADER: &str = "x-csrf-token";
const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
struct NativeHttpState {
    identity: Option<Arc<IdentityService>>,
    api: Option<Arc<NativeApiService>>,
    secure_cookie: bool,
    required_ready: bool,
    retention: Option<RetentionCapability>,
    project_deletion: Option<ProjectDeletionCapability>,
}

#[derive(Debug, Clone, Copy)]
pub struct RetentionCapability {
    pub events_days: u32,
    pub issue_stats_hourly_days: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectDeletionCapability {
    pub grace_period_seconds: u64,
    pub delete_batch_documents: usize,
    pub slug_reservation_seconds: u64,
}

#[derive(Debug)]
enum HttpApiError {
    Api(NativeApiError),
    InvalidRequest,
    InvalidCredentials,
    CsrfFailed,
    Unavailable,
}

impl IntoResponse for HttpApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::InvalidRequest => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "request is invalid",
            ),
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "authentication failed",
            ),
            Self::CsrfFailed => (
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF validation failed",
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "service is temporarily unavailable",
            ),
            Self::Api(error) => {
                let status = match error {
                    NativeApiError::InvalidRequest | NativeApiError::InvalidCursor => {
                        StatusCode::BAD_REQUEST
                    }
                    NativeApiError::InvalidCredentials => StatusCode::UNAUTHORIZED,
                    NativeApiError::Forbidden => StatusCode::FORBIDDEN,
                    NativeApiError::NotFound => StatusCode::NOT_FOUND,
                    NativeApiError::Conflict => StatusCode::CONFLICT,
                    NativeApiError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                    NativeApiError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
                    NativeApiError::Search(search) => match search {
                        SearchError::Syntax | SearchError::InvalidCursor => StatusCode::BAD_REQUEST,
                        SearchError::FieldNotIndexed
                        | SearchError::LimitExceeded
                        | SearchError::PositiveAnchorRequired
                        | SearchError::TooBroad => StatusCode::UNPROCESSABLE_ENTITY,
                        SearchError::NotFound => StatusCode::NOT_FOUND,
                        SearchError::Unavailable | SearchError::InvalidData => {
                            StatusCode::SERVICE_UNAVAILABLE
                        }
                    },
                };
                (status, error.code(), public_message(error))
            }
        };
        let mut response = (
            status,
            Json(json!({
                "error": {
                    "code": code,
                    "message": message,
                    "request_id": "unknown",
                }
            })),
        )
            .into_response();
        response
            .headers_mut()
            .insert("x-faultkeep-error-code", HeaderValue::from_static(code));
        response.headers_mut().insert(
            "x-faultkeep-error-message",
            HeaderValue::from_static(message),
        );
        response
    }
}

fn public_message(error: &NativeApiError) -> &'static str {
    match error {
        NativeApiError::InvalidRequest => "request is invalid",
        NativeApiError::InvalidCursor => "cursor is invalid",
        NativeApiError::InvalidCredentials => "authentication failed",
        NativeApiError::Forbidden => "request is forbidden",
        NativeApiError::NotFound => "target was not found",
        NativeApiError::Conflict => "request conflicts with current state",
        NativeApiError::RateLimited => "request is rate limited",
        NativeApiError::Search(_) => "search request cannot be completed",
        NativeApiError::Unavailable => "service is temporarily unavailable",
    }
}

pub fn router(
    identity: Option<Arc<IdentityService>>,
    api: Option<Arc<NativeApiService>>,
    secure_cookie: bool,
    required_ready: bool,
    retention: Option<RetentionCapability>,
    project_deletion: Option<ProjectDeletionCapability>,
) -> Router {
    let state = NativeHttpState {
        identity,
        api,
        secure_cookie,
        required_ready,
        retention,
        project_deletion,
    };
    Router::new()
        .route("/api/v1/auth/bootstrap", post(bootstrap))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(current_identity))
        .route("/api/v1/auth/tokens", get(list_tokens).post(create_token))
        .route("/api/v1/auth/tokens/{token_id}", delete(revoke_token))
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{project_id}",
            get(get_project).delete(request_project_deletion),
        )
        .route(
            "/api/v1/projects/{project_id}/deletion",
            get(project_deletion_status),
        )
        .route(
            "/api/v1/projects/{project_id}/deletion/cancel",
            post(cancel_project_deletion),
        )
        .route(
            "/api/v1/projects/{project_id}/keys",
            get(list_project_keys).post(create_project_key),
        )
        .route(
            "/api/v1/projects/{project_id}/keys/{dsn_key}",
            delete(disable_project_key),
        )
        .route(
            "/api/v1/projects/{project_id}/policy",
            get(get_project_policy).patch(update_project_policy),
        )
        .route("/api/v1/projects/{project_id}/issues", get(list_issues))
        .route(
            "/api/v1/projects/{project_id}/issues/{issue_id}",
            get(get_issue),
        )
        .route(
            "/api/v1/projects/{project_id}/issues/{issue_id}/lifecycle",
            post(issue_lifecycle),
        )
        .route(
            "/api/v1/projects/{project_id}/issues/{issue_id}/statistics",
            get(issue_statistics),
        )
        .route(
            "/api/v1/projects/{project_id}/issues/{issue_id}/activity",
            get(issue_activity),
        )
        .route(
            "/api/v1/projects/{project_id}/issues/{issue_id}/events",
            get(issue_events),
        )
        .route(
            "/api/v1/projects/{project_id}/events/search",
            get(search_events),
        )
        .route(
            "/api/v1/projects/{project_id}/events/{event_id}",
            get(get_event),
        )
        .route("/api/v1/projects/{project_id}/events", get(list_events))
        .route("/api/v1/projects/{project_id}/releases", get(list_releases))
        .route(
            "/api/v1/projects/{project_id}/environments",
            get(list_environments),
        )
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/status", get(component_status))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(native_error_context))
        .with_state(state)
}

async fn native_error_context(request: Request, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(ToString::to_string)
        .unwrap_or_else(|| "unknown".to_owned());
    let response = next.run(request).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }
    let (mut parts, _) = response.into_parts();
    let code = parts
        .headers
        .remove("x-faultkeep-error-code")
        .and_then(|value| value.to_str().ok().map(str::to_owned))
        .unwrap_or_else(|| "invalid_request".to_owned());
    let message = parts
        .headers
        .remove("x-faultkeep-error-message")
        .and_then(|value| value.to_str().ok().map(str::to_owned))
        .unwrap_or_else(|| "request is invalid".to_owned());
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = serde_json::to_vec(&json!({
        "error": {
            "code": code,
            "message": message,
            "request_id": request_id,
        }
    }))
    .unwrap_or_else(|_| br#"{"error":{"code":"temporarily_unavailable","message":"service is temporarily unavailable","request_id":"unknown"}}"#.to_vec());
    Response::from_parts(parts, axum::body::Body::from(body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapBody {
    setup_token: String,
    email: String,
    display_name: String,
    password: String,
    organization_slug: String,
    organization_name: String,
}

async fn bootstrap(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    body: Result<Json<BootstrapBody>, JsonRejection>,
) -> Result<Json<Value>, HttpApiError> {
    let body = json_body(body)?;
    let identity = identity(&state)?;
    let context = identity
        .bootstrap(BootstrapRequest {
            setup_secret: secret(&body.setup_token)?,
            email: EmailAddress::parse(body.email).map_err(|_| HttpApiError::InvalidRequest)?,
            user_display_name: UserDisplayName::new(body.display_name)
                .map_err(|_| HttpApiError::InvalidRequest)?,
            password: PasswordInput::new(body.password)
                .map_err(|_| HttpApiError::InvalidRequest)?,
            organization_slug: Slug::new(body.organization_slug)
                .map_err(|_| HttpApiError::InvalidRequest)?,
            organization_name: DisplayName::new(body.organization_name)
                .map_err(|_| HttpApiError::InvalidRequest)?,
            request_id: correlation_id(request_id)?,
        })
        .await
        .map_err(|error| HttpApiError::Api(map_auth(error)))?;
    Ok(Json(context_value(&context)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    email: String,
    password: String,
    organization_id: LoginOrganizationId,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LoginOrganizationId {
    Decimal(String),
    LegacyNumber(u64),
}

impl LoginOrganizationId {
    fn parse(self) -> Result<faultkeep_domain::OrganizationId, HttpApiError> {
        let value = match self {
            Self::Decimal(value) => value
                .parse::<u64>()
                .map_err(|_| HttpApiError::InvalidRequest)?,
            Self::LegacyNumber(value) => value,
        };
        faultkeep_domain::OrganizationId::new(value).map_err(|_| HttpApiError::InvalidRequest)
    }
}

async fn login(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Result<Json<LoginBody>, JsonRejection>,
) -> Result<Response, HttpApiError> {
    let body = json_body(body)?;
    let organization_id = body.organization_id.parse()?;
    let issued = identity(&state)?
        .login(LoginRequest {
            email: body.email.into(),
            password: body.password.into(),
            organization_id,
            client_network_digest: network_digest(Some(peer)),
            request_id: correlation_id(request_id)?,
        })
        .await
        .map_err(|error| HttpApiError::Api(map_auth(error)))?;
    let mut response = Json(json!({
        "csrf_token": issued.csrf.encode_hex(),
        "expires_at": timestamp_string(issued.absolute_expires_at)?,
    }))
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&session_cookie(&issued.session, state.secure_cookie, false))
            .map_err(|_| HttpApiError::Unavailable)?,
    );
    Ok(response)
}

async fn logout(
    State(state): State<NativeHttpState>,
    headers: HeaderMap,
) -> Result<Response, HttpApiError> {
    let _ = authenticate(&state, &headers, true).await?;
    let session = session_secret(&headers).ok_or(HttpApiError::InvalidCredentials)?;
    identity(&state)?
        .logout(&session)
        .await
        .map_err(|error| HttpApiError::Api(map_auth(error)))?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&session_cookie(&session, state.secure_cookie, true))
            .map_err(|_| HttpApiError::Unavailable)?,
    );
    Ok(response)
}

async fn current_identity(
    State(state): State<NativeHttpState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    Ok(Json(context_value(&context)))
}

async fn list_tokens(
    State(state): State<NativeHttpState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let tokens = api(&state)?
        .api_tokens(&context)
        .await
        .map_err(HttpApiError::Api)?;
    let items = tokens
        .into_iter()
        .map(|token| {
            Ok(json!({
                "id": token.id.get().to_string(),
                "name": token.name,
                "scopes": token.scopes,
                "created_at": timestamp_string(token.created_at)?,
                "expires_at": timestamp_string(token.expires_at)?,
                "last_used_at": optional_timestamp(token.last_used_at),
            }))
        })
        .collect::<Result<Vec<_>, HttpApiError>>()?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTokenBody {
    name: String,
    scopes: Vec<String>,
    expires_at: String,
}

async fn create_token(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<CreateTokenBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let body = json_body(body)?;
    let scopes = body
        .scopes
        .iter()
        .map(|scope| Permission::parse_scope(scope))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| HttpApiError::InvalidRequest)?;
    let token = identity(&state)?
        .create_api_token(
            &context,
            CreateApiTokenRequest {
                name: TokenName::new(body.name).map_err(|_| HttpApiError::InvalidRequest)?,
                scopes: PermissionSet::from_permissions(scopes),
                expires_at: parse_timestamp(&body.expires_at)?,
                request_id: correlation_id(request_id)?,
            },
        )
        .await
        .map_err(|error| HttpApiError::Api(map_auth(error)))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": token.id.get().to_string(),
            "token": token.secret.encode_hex(),
            "expires_at": timestamp_string(token.expires_at)?,
        })),
    ))
}

async fn revoke_token(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    Path(token_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let token_id = CredentialId::new(
        token_id
            .parse::<u64>()
            .map_err(|_| HttpApiError::InvalidRequest)?,
    )
    .map_err(|_| HttpApiError::InvalidRequest)?;
    identity(&state)?
        .revoke_api_token(&context, token_id, correlation_id(request_id)?)
        .await
        .map_err(|error| HttpApiError::Api(map_auth(error)))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_projects(
    State(state): State<NativeHttpState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let projects = api(&state)?
        .list_projects(&context)
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": projects.iter().map(project_value).collect::<Result<Vec<_>, _>>()?
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectBody {
    slug: String,
    display_name: String,
    #[serde(default = "default_ip_policy")]
    ip_policy: String,
    #[serde(default = "default_true")]
    error_enabled: bool,
    #[serde(default = "default_true")]
    client_report_enabled: bool,
    #[serde(default = "default_event_bytes")]
    max_event_bytes: u32,
    max_events_per_second: Option<u32>,
    burst: Option<u32>,
}

async fn create_project(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<ProjectBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let body = json_body(body)?;
    let created = api(&state)?
        .create_project(
            &context,
            CreateProject {
                organization_id: context.organization_id,
                slug: Slug::new(body.slug).map_err(|_| HttpApiError::InvalidRequest)?,
                display_name: DisplayName::new(body.display_name)
                    .map_err(|_| HttpApiError::InvalidRequest)?,
                ip_policy: ip_policy(&body.ip_policy)?,
                items: ItemCapabilities {
                    error: body.error_enabled,
                    client_report: body.client_report_enabled,
                },
                limits: ingest_limits(
                    body.max_event_bytes,
                    body.max_events_per_second,
                    body.burst,
                )?,
            },
            correlation_id(request_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "project_id": created.project_id.get().to_string(),
            "dsn_key": created.dsn_key.to_string(),
        })),
    ))
}

async fn get_project(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let project = api(&state)?
        .project(&context, project_id_from(&project_id)?)
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(project_value(&project)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteProjectBody {
    confirm_slug: String,
}

async fn request_project_deletion(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DeleteProjectBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let operation_id = deletion_operation_header(&headers)?;
    let body = json_body(body)?;
    let status = api(&state)?
        .request_project_deletion(
            &context,
            project_id_from(&project_id)?,
            operation_id,
            &body.confirm_slug,
            correlation_id(request_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok((StatusCode::ACCEPTED, Json(deletion_status_value(&status)?)))
}

async fn project_deletion_status(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let status = api(&state)?
        .project_deletion_status(&context, project_id_from(&project_id)?)
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(deletion_status_value(&status)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelDeletionBody {
    operation_id: String,
}

async fn cancel_project_deletion(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<CancelDeletionBody>, JsonRejection>,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let body = json_body(body)?;
    let status = api(&state)?
        .cancel_project_deletion(
            &context,
            project_id_from(&project_id)?,
            ProjectDeletionOperationId::from_bytes(hex_16(&body.operation_id)?),
            correlation_id(request_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(deletion_status_value(&status)?))
}

async fn list_project_keys(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let keys = api(&state)?
        .project_keys(&context, project_id_from(&project_id)?)
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": keys.iter().map(project_key_value).collect::<Result<Vec<_>, _>>()?
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyBody {
    label: String,
}

async fn create_project_key(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<KeyBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let body = json_body(body)?;
    let key = api(&state)?
        .create_project_key(
            &context,
            project_id_from(&project_id)?,
            ProjectKeyLabel::new(body.label).map_err(|_| HttpApiError::InvalidRequest)?,
            correlation_id(request_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "dsn_key": key.to_string() })),
    ))
}

async fn disable_project_key(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    Path((project_id, dsn_key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    api(&state)?
        .disable_project_key(
            &context,
            project_id_from(&project_id)?,
            DsnKey::parse(&dsn_key).map_err(|_| HttpApiError::InvalidRequest)?,
            correlation_id(request_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_project_policy(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let project = api(&state)?
        .project(&context, project_id_from(&project_id)?)
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(policy_value(&project)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyBody {
    expected_revision: u64,
    ip_policy: String,
    error_enabled: bool,
    client_report_enabled: bool,
    max_event_bytes: u32,
    max_events_per_second: Option<u32>,
    burst: Option<u32>,
}

async fn update_project_policy(
    State(state): State<NativeHttpState>,
    Extension(request_id): Extension<RequestId>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<PolicyBody>, JsonRejection>,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let body = json_body(body)?;
    let project = api(&state)?
        .update_project_policy(
            &context,
            project_id_from(&project_id)?,
            ProjectPolicyUpdate {
                expected_revision: body.expected_revision,
                ip_policy: ip_policy(&body.ip_policy)?,
                items: ItemCapabilities {
                    error: body.error_enabled,
                    client_report: body.client_report_enabled,
                },
                limits: ingest_limits(
                    body.max_event_bytes,
                    body.max_events_per_second,
                    body.burst,
                )?,
            },
            correlation_id(request_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(policy_value(&project)))
}

async fn list_issues(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let query = query_map(raw.as_deref())?;
    let status = query
        .get("status")
        .map(|value| issue_status(value))
        .transpose()?;
    let page = api(&state)?
        .list_issues(
            &context,
            project_id_from(&project_id)?,
            status,
            query.get("cursor").map(String::as_str),
            query_limit(&query)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": page.items.iter().map(issue_value).collect::<Result<Vec<_>, _>>()?,
        "next_cursor": page.next_cursor,
    })))
}

async fn get_issue(
    State(state): State<NativeHttpState>,
    Path((project_id, issue_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let issue = api(&state)?
        .issue(
            &context,
            project_id_from(&project_id)?,
            issue_id_from(&issue_id)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(issue_value(&issue)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleBody {
    action: String,
    idempotency_key: String,
    assignee_user_id: Option<u64>,
}

async fn issue_lifecycle(
    State(state): State<NativeHttpState>,
    Path((project_id, issue_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<LifecycleBody>, JsonRejection>,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, true).await?;
    let body = json_body(body)?;
    let action = lifecycle_action(&body.action, body.assignee_user_id)?;
    let result = api(&state)?
        .issue_command(
            &context,
            project_id_from(&project_id)?,
            issue_id_from(&issue_id)?,
            hex_16(&body.idempotency_key)?,
            action,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "applied": result.applied,
        "issue": issue_value(&result.issue)?,
    })))
}

async fn issue_statistics(
    State(state): State<NativeHttpState>,
    Path((project_id, issue_id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let query = query_map(raw.as_deref())?;
    let values = api(&state)?
        .issue_statistics(
            &context,
            project_id_from(&project_id)?,
            issue_id_from(&issue_id)?,
            optional_query_timestamp(&query, "from")?,
            optional_query_timestamp(&query, "until")?,
            query_limit(&query)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    let items = values
        .into_iter()
        .map(|value| {
            Ok(json!({
                "bucket_start": timestamp_string(value.bucket_start)?,
                "occurrence_count": value.occurrence_count.get(),
                "approximate": true,
            }))
        })
        .collect::<Result<Vec<_>, HttpApiError>>()?;
    Ok(Json(json!({ "items": items })))
}

async fn issue_activity(
    State(state): State<NativeHttpState>,
    Path((project_id, issue_id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let query = query_map(raw.as_deref())?;
    let page = api(&state)?
        .issue_activity(
            &context,
            project_id_from(&project_id)?,
            issue_id_from(&issue_id)?,
            query.get("cursor").map(String::as_str),
            query_limit(&query)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": page.items.iter().map(activity_value).collect::<Result<Vec<_>, _>>()?,
        "next_cursor": page.next_cursor,
    })))
}

async fn issue_events(
    State(state): State<NativeHttpState>,
    Path((project_id, issue_id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    event_page(
        &state,
        &headers,
        &project_id,
        Some(issue_id_from(&issue_id)?),
        raw.as_deref(),
    )
    .await
}

async fn list_events(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    event_page(&state, &headers, &project_id, None, raw.as_deref()).await
}

async fn event_page(
    state: &NativeHttpState,
    headers: &HeaderMap,
    project_id: &str,
    issue_id: Option<IssueId>,
    raw: Option<&str>,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(state, headers, false).await?;
    let query = query_map(raw)?;
    let page = api(state)?
        .list_events(
            &context,
            project_id_from(project_id)?,
            EventListRequest {
                issue_id,
                from: optional_query_timestamp(&query, "from")?,
                until: optional_query_timestamp(&query, "until")?,
                cursor: query.get("cursor").map(String::as_str),
                limit: query_limit(&query)?,
            },
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": page.items.iter().map(event_value).collect::<Result<Vec<_>, _>>()?,
        "next_cursor": page.next_cursor,
    })))
}

async fn search_events(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let query = query_map(raw.as_deref())?;
    let text = query.get("q").ok_or(HttpApiError::InvalidRequest)?;
    let page = api(&state)?
        .search(
            &context,
            project_id_from(&project_id)?,
            text,
            query.get("cursor").map(String::as_str),
            query_limit(&query)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": page.items.iter().map(event_value).collect::<Result<Vec<_>, _>>()?,
        "next_cursor": page.next_cursor,
        "candidates_examined": page.candidates_examined,
    })))
}

async fn get_event(
    State(state): State<NativeHttpState>,
    Path((project_id, event_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let event = api(&state)?
        .event(
            &context,
            project_id_from(&project_id)?,
            EventId::parse(&event_id).map_err(|_| HttpApiError::InvalidRequest)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(event_value(&event)?))
}

async fn list_releases(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let query = query_map(raw.as_deref())?;
    let page = api(&state)?
        .releases(
            &context,
            project_id_from(&project_id)?,
            query.get("cursor").map(String::as_str),
            query_limit(&query)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": page.items.iter().map(release_value).collect::<Result<Vec<_>, _>>()?,
        "next_cursor": page.next_cursor,
    })))
}

async fn list_environments(
    State(state): State<NativeHttpState>,
    Path(project_id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let context = authenticate(&state, &headers, false).await?;
    let query = query_map(raw.as_deref())?;
    let page = api(&state)?
        .environments(
            &context,
            project_id_from(&project_id)?,
            query.get("cursor").map(String::as_str),
            query_limit(&query)?,
        )
        .await
        .map_err(HttpApiError::Api)?;
    Ok(Json(json!({
        "items": page.items.iter().map(environment_value).collect::<Result<Vec<_>, _>>()?,
        "next_cursor": page.next_cursor,
    })))
}

async fn capabilities(State(state): State<NativeHttpState>) -> Json<Value> {
    let retention = state.retention.map(|policy| {
        json!({
            "events_days": policy.events_days,
            "issue_stats_hourly_days": policy.issue_stats_hourly_days,
            "clock": "received_at",
            "gradual_policy_reduction": true,
        })
    });
    let project_deletion = state.project_deletion.map(|policy| {
        json!({
            "grace_period_seconds": policy.grace_period_seconds,
            "delete_batch_documents": policy.delete_batch_documents,
            "slug_reservation_seconds": policy.slug_reservation_seconds,
            "final_reconciliation": true,
            "filesystem_namespaces": 0,
        })
    });
    Json(json!({
        "api_version": "v1",
        "search": {
            "fields": [
                "event.id", "issue", "timestamp", "level", "platform",
                "environment", "release", "user.id"
            ],
            "full_text": false,
            "custom_tags": false,
            "max_page_size": 100,
        },
        "features": {
            "native_api": true,
            "web": true,
            "retention": retention.is_some(),
            "project_deletion": project_deletion.is_some(),
            "mcp": false,
            "migrations": false,
            "nats": false,
            "sharding": false,
            "disk_spool": false,
        },
        "retention": retention,
        "project_deletion": project_deletion,
    }))
}

async fn component_status(
    State(state): State<NativeHttpState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpApiError> {
    let _ = authenticate(&state, &headers, false).await?;
    Ok(Json(json!({
        "status": if state.required_ready { "ready" } else { "degraded" },
        "components": {
            "mongodb": if state.required_ready { "available" } else { "unavailable" },
            "writer": if state.required_ready { "running" } else { "stopped" },
            "dispatcher": if state.required_ready { "running" } else { "stopped" },
            "processor": if state.required_ready { "running" } else { "stopped" },
            "scheduler": if state.required_ready { "running" } else { "stopped" },
            "project_deletion": if state.project_deletion.is_some() { "running" } else { "stopped" },
            "symbolication": "baseline",
        }
    })))
}

async fn authenticate(
    state: &NativeHttpState,
    headers: &HeaderMap,
    state_changing: bool,
) -> Result<AuthContext, HttpApiError> {
    let identity = identity(state)?;
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value
            .to_str()
            .map_err(|_| HttpApiError::InvalidCredentials)?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or(HttpApiError::InvalidCredentials)?;
        return identity
            .authenticate_api_token(&secret(token)?)
            .await
            .map_err(|error| HttpApiError::Api(map_auth(error)));
    }
    let session = session_secret(headers).ok_or(HttpApiError::InvalidCredentials)?;
    let organization_id = headers
        .get(ORGANIZATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|value| faultkeep_domain::OrganizationId::new(value).ok())
        .ok_or(HttpApiError::InvalidCredentials)?;
    let csrf = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(secret)
        .transpose()?;
    if state_changing && csrf.is_none() {
        return Err(HttpApiError::CsrfFailed);
    }
    identity
        .authenticate_session(&session, csrf.as_ref(), state_changing, organization_id)
        .await
        .map_err(|error| {
            if state_changing
                && matches!(
                    error,
                    faultkeep_application::auth::AuthError::InvalidCredential
                )
            {
                HttpApiError::CsrfFailed
            } else {
                HttpApiError::Api(map_auth(error))
            }
        })
}

fn identity(state: &NativeHttpState) -> Result<&Arc<IdentityService>, HttpApiError> {
    state.identity.as_ref().ok_or(HttpApiError::Unavailable)
}

fn api(state: &NativeHttpState) -> Result<&Arc<NativeApiService>, HttpApiError> {
    state.api.as_ref().ok_or(HttpApiError::Unavailable)
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, HttpApiError> {
    body.map(|Json(value)| value)
        .map_err(|_| HttpApiError::InvalidRequest)
}

fn correlation_id(request_id: RequestId) -> Result<RequestCorrelationId, HttpApiError> {
    BoundedId::new(request_id.to_string()).map_err(|_| HttpApiError::Unavailable)
}

fn secret(value: &str) -> Result<PlainSecret, HttpApiError> {
    let bytes = hex_32(value)?;
    Ok(PlainSecret::new(bytes))
}

fn hex_32(value: &str) -> Result<[u8; 32], HttpApiError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HttpApiError::InvalidCredentials);
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| HttpApiError::InvalidCredentials)?;
    Ok(bytes)
}

fn hex_16(value: &str) -> Result<[u8; 16], HttpApiError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HttpApiError::InvalidRequest);
    }
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| HttpApiError::InvalidRequest)?;
    Ok(bytes)
}

fn deletion_operation_header(
    headers: &HeaderMap,
) -> Result<ProjectDeletionOperationId, HttpApiError> {
    let value = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(HttpApiError::InvalidRequest)?;
    Ok(ProjectDeletionOperationId::from_bytes(hex_16(value)?))
}

fn session_secret(headers: &HeaderMap) -> Option<PlainSecret> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|cookie| {
        let (name, value) = cookie.trim().split_once('=')?;
        (name == SESSION_COOKIE)
            .then(|| secret(value).ok())
            .flatten()
    })
}

fn session_cookie(secret: &PlainSecret, secure: bool, clear: bool) -> String {
    let value = if clear {
        String::new()
    } else {
        secret.encode_hex()
    };
    format!(
        "{SESSION_COOKIE}={value}; Path=/api/v1; HttpOnly; SameSite=Lax{}{}",
        if secure { "; Secure" } else { "" },
        if clear { "; Max-Age=0" } else { "" },
    )
}

fn network_digest(peer: Option<SocketAddr>) -> SecretDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"faultkeep/login-network/v1");
    match peer.map(|value| value.ip()) {
        Some(std::net::IpAddr::V4(value)) => hasher.update(value.octets()),
        Some(std::net::IpAddr::V6(value)) => hasher.update(value.octets()),
        None => hasher.update([0_u8; 16]),
    }
    SecretDigest::new(hasher.finalize().into())
}

fn query_map(raw: Option<&str>) -> Result<BTreeMap<String, String>, HttpApiError> {
    let mut values = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if values
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(HttpApiError::InvalidRequest);
        }
    }
    Ok(values)
}

fn query_limit(query: &BTreeMap<String, String>) -> Result<Option<usize>, HttpApiError> {
    query
        .get("limit")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| HttpApiError::InvalidRequest)
        })
        .transpose()
}

fn optional_query_timestamp(
    query: &BTreeMap<String, String>,
    field: &str,
) -> Result<Option<Timestamp>, HttpApiError> {
    query
        .get(field)
        .map(|value| parse_timestamp(value))
        .transpose()
}

fn parse_timestamp(value: &str) -> Result<Timestamp, HttpApiError> {
    let value = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| HttpApiError::InvalidRequest)?;
    let millis = value.unix_timestamp_nanos().div_euclid(1_000_000);
    i64::try_from(millis)
        .ok()
        .and_then(|value| Timestamp::from_unix_millis(value).ok())
        .ok_or(HttpApiError::InvalidRequest)
}

fn timestamp_string(value: Timestamp) -> Result<String, HttpApiError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value.unix_millis()) * 1_000_000)
        .map_err(|_| HttpApiError::Unavailable)?
        .format(&Rfc3339)
        .map_err(|_| HttpApiError::Unavailable)
}

fn optional_timestamp(value: Option<Timestamp>) -> Value {
    value
        .and_then(|value| timestamp_string(value).ok())
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn project_id_from(value: &str) -> Result<ProjectId, HttpApiError> {
    value
        .parse::<i32>()
        .ok()
        .and_then(|value| ProjectId::new(value).ok())
        .ok_or(HttpApiError::InvalidRequest)
}

fn issue_id_from(value: &str) -> Result<IssueId, HttpApiError> {
    Ok(IssueId::from_bytes(hex_16(value)?))
}

fn issue_status(value: &str) -> Result<IssueStatus, HttpApiError> {
    match value {
        "open" => Ok(IssueStatus::Open),
        "resolved" => Ok(IssueStatus::Resolved),
        "ignored" => Ok(IssueStatus::Ignored),
        _ => Err(HttpApiError::InvalidRequest),
    }
}

fn lifecycle_action(
    action: &str,
    assignee_user_id: Option<u64>,
) -> Result<IssueCommandAction, HttpApiError> {
    match action {
        "resolve" => Ok(IssueCommandAction::Resolve),
        "ignore" => Ok(IssueCommandAction::Ignore),
        "reopen" => Ok(IssueCommandAction::Reopen),
        "unassign" => Ok(IssueCommandAction::Assign(None)),
        "assign" => {
            let value = assignee_user_id.ok_or(HttpApiError::InvalidRequest)?;
            let mut id = [0_u8; 16];
            id[8..].copy_from_slice(&value.to_be_bytes());
            Ok(IssueCommandAction::Assign(Some(ActorRef::new(
                ActorKind::User,
                id,
            ))))
        }
        _ => Err(HttpApiError::InvalidRequest),
    }
}

fn ingest_limits(
    max_event_bytes: u32,
    max_events_per_second: Option<u32>,
    burst: Option<u32>,
) -> Result<ProjectIngestLimits, HttpApiError> {
    Ok(ProjectIngestLimits {
        max_event_bytes: NonZeroU32::new(max_event_bytes)
            .filter(|value| value.get() <= 20 * 1024 * 1024)
            .ok_or(HttpApiError::InvalidRequest)?,
        max_events_per_second: max_events_per_second
            .map(|value| NonZeroU32::new(value).ok_or(HttpApiError::InvalidRequest))
            .transpose()?,
        burst: burst
            .map(|value| NonZeroU32::new(value).ok_or(HttpApiError::InvalidRequest))
            .transpose()?,
    })
}

fn ip_policy(value: &str) -> Result<IpScrubPolicy, HttpApiError> {
    match value {
        "hmac" => Ok(IpScrubPolicy::Hmac),
        "keep" => Ok(IpScrubPolicy::Keep),
        "remove" => Ok(IpScrubPolicy::Remove),
        "truncate" => Ok(IpScrubPolicy::Truncate),
        _ => Err(HttpApiError::InvalidRequest),
    }
}

fn default_ip_policy() -> String {
    "hmac".to_owned()
}

const fn default_true() -> bool {
    true
}

const fn default_event_bytes() -> u32 {
    1024 * 1024
}

fn map_auth(error: faultkeep_application::auth::AuthError) -> NativeApiError {
    use faultkeep_application::auth::AuthError;
    match error {
        AuthError::Forbidden => NativeApiError::Forbidden,
        AuthError::InvalidCredentials | AuthError::InvalidCredential => {
            NativeApiError::InvalidCredentials
        }
        AuthError::RateLimited => NativeApiError::RateLimited,
        AuthError::AlreadyExists | AuthError::FinalOwner => NativeApiError::Conflict,
        AuthError::NotFound => NativeApiError::NotFound,
        AuthError::InvalidPassword | AuthError::InvalidTokenPolicy => {
            NativeApiError::InvalidRequest
        }
        _ => NativeApiError::Unavailable,
    }
}

fn context_value(context: &AuthContext) -> Value {
    json!({
        "actor": match context.actor {
            Actor::WebSession => "web_session",
            Actor::PersonalApiToken => "personal_api_token",
            Actor::Bootstrap => "bootstrap",
        },
        "user_id": context.user_id.get().to_string(),
        "organization_id": context.organization_id.get().to_string(),
        "role": role_name(context.role),
        "permissions": context.permissions.iter().map(Permission::scope).collect::<Vec<_>>(),
        "credential_id": context.credential_id.get().to_string(),
    })
}

const fn role_name(role: OrganizationRole) -> &'static str {
    match role {
        OrganizationRole::Owner => "owner",
        OrganizationRole::Admin => "admin",
        OrganizationRole::Member => "member",
        OrganizationRole::Viewer => "viewer",
    }
}

fn project_value(project: &ProjectView) -> Result<Value, HttpApiError> {
    Ok(json!({
        "id": project.id.get().to_string(),
        "organization_id": project.organization_id.get().to_string(),
        "slug": project.slug.as_str(),
        "display_name": project.display_name.as_str(),
        "state": project_state(project),
        "policy": policy_value(project),
        "grouping_revision": project.grouping_revision,
        "created_at": timestamp_string(project.created_at)?,
    }))
}

fn deletion_status_value(status: &ProjectDeletionStatus) -> Result<Value, HttpApiError> {
    Ok(json!({
        "operation_id": hex::encode(status.operation_id.as_bytes()),
        "project_id": status.project_id.get().to_string(),
        "organization_id": status.organization_id.get().to_string(),
        "phase": match status.phase {
            ProjectDeletionPhase::PendingGrace => "pending_grace",
            ProjectDeletionPhase::Purging => "purging",
            ProjectDeletionPhase::Deleted => "deleted",
            ProjectDeletionPhase::Cancelled => "cancelled",
        },
        "dataset_code": status.dataset_code,
        "reconciliation_pass": status.reconciliation_pass,
        "requested_at": timestamp_string(status.requested_at)?,
        "purge_after": timestamp_string(status.purge_after)?,
        "completed_at": status.completed_at.map(timestamp_string).transpose()?,
        "next_attempt_at": timestamp_string(status.next_attempt_at)?,
        "attempts": status.attempts,
        "last_error": status.last_error,
        "status_url": format!("/api/v1/projects/{}/deletion", status.project_id.get()),
    }))
}

fn project_state(project: &ProjectView) -> &'static str {
    match project.state {
        faultkeep_domain::ProjectAcceptanceState::Active => "active",
        faultkeep_domain::ProjectAcceptanceState::Disabled => "disabled",
        faultkeep_domain::ProjectAcceptanceState::PendingDelete => "pending_delete",
        faultkeep_domain::ProjectAcceptanceState::Purging => "purging",
        faultkeep_domain::ProjectAcceptanceState::Deleted => "deleted",
    }
}

fn policy_value(project: &ProjectView) -> Value {
    json!({
        "revision": project.policy_revision,
        "ip_policy": match project.ip_policy {
            IpScrubPolicy::Hmac => "hmac",
            IpScrubPolicy::Keep => "keep",
            IpScrubPolicy::Remove => "remove",
            IpScrubPolicy::Truncate => "truncate",
        },
        "items": {
            "error": project.items.error,
            "client_report": project.items.client_report,
        },
        "limits": {
            "max_event_bytes": project.limits.max_event_bytes.get(),
            "max_events_per_second": project.limits.max_events_per_second.map(NonZeroU32::get),
            "burst": project.limits.burst.map(NonZeroU32::get),
        }
    })
}

fn project_key_value(key: &ProjectKeyView) -> Result<Value, HttpApiError> {
    Ok(json!({
        "dsn_key": key.key.to_string(),
        "project_id": key.project_id.get().to_string(),
        "state": match key.state {
            faultkeep_domain::ProjectKeyState::Active => "active",
            faultkeep_domain::ProjectKeyState::Disabled => "disabled",
            faultkeep_domain::ProjectKeyState::SuspendedByDeletion => "suspended_by_deletion",
        },
        "label": key.label.as_str(),
        "created_at": timestamp_string(key.created_at)?,
    }))
}

fn issue_value(issue: &IssueSnapshot) -> Result<Value, HttpApiError> {
    Ok(json!({
        "id": issue.issue_id.to_string(),
        "project_id": issue.project_id.get().to_string(),
        "title": issue.title.as_str(),
        "culprit": issue.culprit.as_ref().map(|value| value.as_str()),
        "status": match issue.status {
            IssueStatus::Open => "open",
            IssueStatus::Resolved => "resolved",
            IssueStatus::Ignored => "ignored",
        },
        "first_seen": timestamp_string(issue.first_seen)?,
        "last_seen": timestamp_string(issue.last_seen)?,
        "first_event_id": issue.first_event_id.to_string(),
        "latest_event_id": issue.latest_event_id.to_string(),
        "representative_event_id": issue.representative_event_id.to_string(),
        "occurrence_count": issue.occurrence_count.get(),
        "occurrence_count_approximate": true,
        "assignee": issue.assignee.map(actor_value),
        "first_release": issue.first_release.as_ref().map(|value| value.as_str()),
        "last_release": issue.last_release.as_ref().map(|value| value.as_str()),
        "grouping": {
            "strategy": issue.grouping.strategy.as_str(),
            "summary": issue.grouping.explanation.summary,
        },
    }))
}

fn actor_value(actor: ActorRef) -> Value {
    json!({
        "kind": match actor.kind() {
            ActorKind::User => "user",
            ActorKind::ApiCredential => "api_credential",
            ActorKind::System => "system",
        },
        "id": hex::encode(actor.id()),
    })
}

fn event_value(event: &EventView) -> Result<Value, HttpApiError> {
    let payload: Value =
        serde_json::from_slice(event.payload.as_bytes()).map_err(|_| HttpApiError::Unavailable)?;
    Ok(json!({
        "event_id": event.key.event_id().to_string(),
        "project_id": event.key.project_id().get().to_string(),
        "issue_id": event.issue_id.to_string(),
        "received_at": timestamp_string(event.received_at)?,
        "occurred_at": timestamp_string(event.occurred_at)?,
        "level": event.level.as_str(),
        "platform": event.platform.as_str(),
        "body": payload,
    }))
}

fn activity_value(activity: &IssueActivityView) -> Result<Value, HttpApiError> {
    Ok(json!({
        "id": hex::encode(activity.id.as_bytes()),
        "issue_id": activity.issue_id.to_string(),
        "kind": match activity.kind {
            IssueActivityKind::Resolved => "resolved",
            IssueActivityKind::Ignored => "ignored",
            IssueActivityKind::Reopened => "reopened",
            IssueActivityKind::Assigned => "assigned",
            IssueActivityKind::Unassigned => "unassigned",
            IssueActivityKind::Regressed => "regressed",
        },
        "actor": actor_value(activity.actor),
        "event_id": activity.event_key.map(|value| value.event_id().to_string()),
        "at": timestamp_string(activity.at)?,
    }))
}

fn release_value(release: &ReleaseView) -> Result<Value, HttpApiError> {
    Ok(json!({
        "id": hex::encode(release.id.as_bytes()),
        "version": release.version,
        "first_seen": timestamp_string(release.first_seen)?,
        "last_seen": timestamp_string(release.last_seen)?,
    }))
}

fn environment_value(environment: &EnvironmentView) -> Result<Value, HttpApiError> {
    Ok(json!({
        "id": hex::encode(environment.id.as_bytes()),
        "name": environment.name,
        "first_seen": timestamp_string(environment.first_seen)?,
        "last_seen": timestamp_string(environment.last_seen)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RouteAccess {
        Public,
        Authenticated,
        Permission(Permission),
    }

    #[test]
    fn descriptive_dto_and_error_envelope_are_golden() {
        let response = HttpApiError::InvalidRequest.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let project = ProjectView {
            id: ProjectId::new(7).unwrap(),
            organization_id: faultkeep_domain::OrganizationId::new(9).unwrap(),
            slug: Slug::new("backend").unwrap(),
            display_name: DisplayName::new("Backend").unwrap(),
            state: faultkeep_domain::ProjectAcceptanceState::Active,
            policy_revision: 2,
            ip_policy: IpScrubPolicy::Hmac,
            items: ItemCapabilities {
                error: true,
                client_report: true,
            },
            limits: ProjectIngestLimits::default(),
            grouping_revision: 1,
            created_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        };
        assert_eq!(
            serde_json::to_string(&project_value(&project).unwrap()).unwrap(),
            r#"{"id":"7","organization_id":"9","slug":"backend","display_name":"Backend","state":"active","policy":{"revision":2,"ip_policy":"hmac","items":{"error":true,"client_report":true},"limits":{"max_event_bytes":1048576,"max_events_per_second":null,"burst":null}},"grouping_revision":1,"created_at":"2023-11-14T22:13:20Z"}"#
        );
    }

    #[test]
    fn query_parser_rejects_duplicates_and_cookie_parser_is_exact() {
        assert!(query_map(Some("limit=10&limit=20")).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "other=x; faultkeep_session=0101010101010101010101010101010101010101010101010101010101010101",
            ),
        );
        assert!(session_secret(&headers).is_some());
    }

    #[test]
    fn login_organization_id_preserves_large_decimal_strings_and_legacy_numbers() {
        let large = 9_007_199_254_740_993_u64;
        let body: LoginBody = serde_json::from_value(json!({
            "email": "owner@example.com",
            "password": "correct horse battery staple",
            "organization_id": large.to_string(),
        }))
        .unwrap();
        assert_eq!(body.organization_id.parse().unwrap().get(), large);

        let legacy: LoginBody = serde_json::from_value(json!({
            "email": "owner@example.com",
            "password": "correct horse battery staple",
            "organization_id": 7,
        }))
        .unwrap();
        assert_eq!(legacy.organization_id.parse().unwrap().get(), 7);
    }

    #[test]
    fn every_native_route_has_a_pinned_permission_contract() {
        let matrix = [
            ("POST /auth/bootstrap", RouteAccess::Public),
            ("POST /auth/login", RouteAccess::Public),
            ("POST /auth/logout", RouteAccess::Authenticated),
            ("GET /auth/me", RouteAccess::Authenticated),
            ("GET /auth/tokens", RouteAccess::Authenticated),
            ("POST /auth/tokens", RouteAccess::Authenticated),
            ("DELETE /auth/tokens/:id", RouteAccess::Authenticated),
            (
                "GET /projects",
                RouteAccess::Permission(Permission::ProjectRead),
            ),
            (
                "POST /projects",
                RouteAccess::Permission(Permission::OrganizationAdmin),
            ),
            (
                "GET /projects/:id",
                RouteAccess::Permission(Permission::ProjectRead),
            ),
            (
                "DELETE /projects/:id",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "GET /projects/:id/deletion",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "POST /projects/:id/deletion/cancel",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "GET /projects/:id/keys",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "POST /projects/:id/keys",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "DELETE /projects/:id/keys/:key",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "GET /projects/:id/policy",
                RouteAccess::Permission(Permission::ProjectRead),
            ),
            (
                "PATCH /projects/:id/policy",
                RouteAccess::Permission(Permission::ProjectAdmin),
            ),
            (
                "GET /projects/:id/issues",
                RouteAccess::Permission(Permission::IssueRead),
            ),
            (
                "GET /projects/:id/issues/:issue",
                RouteAccess::Permission(Permission::IssueRead),
            ),
            (
                "POST /projects/:id/issues/:issue/lifecycle",
                RouteAccess::Permission(Permission::IssueWrite),
            ),
            (
                "GET /projects/:id/issues/:issue/statistics",
                RouteAccess::Permission(Permission::IssueRead),
            ),
            (
                "GET /projects/:id/issues/:issue/activity",
                RouteAccess::Permission(Permission::IssueRead),
            ),
            (
                "GET /projects/:id/issues/:issue/events",
                RouteAccess::Permission(Permission::EventRead),
            ),
            (
                "GET /projects/:id/events",
                RouteAccess::Permission(Permission::EventRead),
            ),
            (
                "GET /projects/:id/events/:event",
                RouteAccess::Permission(Permission::EventRead),
            ),
            (
                "GET /projects/:id/events/search",
                RouteAccess::Permission(Permission::EventRead),
            ),
            (
                "GET /projects/:id/releases",
                RouteAccess::Permission(Permission::ProjectRead),
            ),
            (
                "GET /projects/:id/environments",
                RouteAccess::Permission(Permission::ProjectRead),
            ),
            ("GET /capabilities", RouteAccess::Public),
            ("GET /status", RouteAccess::Authenticated),
        ];
        assert_eq!(matrix.len(), 31);
        let unique = matrix
            .iter()
            .map(|(route, _)| *route)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), matrix.len());
        assert_eq!(
            matrix
                .iter()
                .find(|(route, _)| *route == "POST /projects")
                .map(|(_, access)| *access),
            Some(RouteAccess::Permission(Permission::OrganizationAdmin))
        );
    }
}
