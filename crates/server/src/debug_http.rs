//! Sentry CLI DIF upload and private Symbolicator source HTTP adapters.

use std::{collections::BTreeMap, io::Read, net::SocketAddr, sync::Arc};

use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Path, Query, RawQuery, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use flate2::read::GzDecoder;
use metric_application::{
    artifacts::{ArtifactError, ArtifactService, AssembleArtifact, AssembleArtifactState},
    auth::IdentityService,
    debug_files::{
        AssembleDebugFile, AssembleState, DebugFileError, DebugFileService, SENTRY_CLI_CHUNK_BYTES,
        SENTRY_CLI_CHUNKS_PER_REQUEST, SENTRY_CLI_MAX_REQUEST_BYTES,
    },
};
use metric_domain::{
    ProjectId,
    artifacts::ArtifactBundleId,
    auth::{AuthContext, Permission, PlainSecret},
    debug_files::{CodeId, DebugFileId, DebugId},
};
use metric_symbolication::PrivateSourceSigner;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::TrustedProxy;
use crate::debug_http::DebugHttpError::{InvalidRequest, Unauthorized};

const MAX_ASSEMBLE_BODY_BYTES: usize = 1024 * 1024;
/// Clients fill `maxRequestSize` with chunk bytes alone, so the request limit adds
/// 1 KiB per part for the multipart boundary and part headers.
const MAX_CHUNK_REQUEST_BYTES: usize =
    SENTRY_CLI_MAX_REQUEST_BYTES + SENTRY_CLI_CHUNKS_PER_REQUEST * 1024;

#[derive(Clone)]
struct DebugHttpState {
    identity: Arc<IdentityService>,
    debug_files: Arc<DebugFileService>,
    artifacts: Option<Arc<ArtifactService>>,
    signer: PrivateSourceSigner,
    trusted_proxies: Arc<[TrustedProxy]>,
}

#[derive(Debug)]
enum DebugHttpError {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Quota,
    Conflict,
    Unavailable,
}

impl IntoResponse for DebugHttpError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::InvalidRequest => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "debug file request is invalid",
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "authentication failed",
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "request is forbidden"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", "target was not found"),
            Self::Quota => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "quota_exceeded",
                "debug file quota is exhausted",
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "conflict",
                "debug file conflicts with existing data",
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "debug file service is temporarily unavailable",
            ),
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

pub fn router(
    identity: Option<Arc<IdentityService>>,
    debug_files: Option<Arc<DebugFileService>>,
    artifacts: Option<Arc<ArtifactService>>,
    signer: Option<PrivateSourceSigner>,
    trusted_proxies: Vec<TrustedProxy>,
) -> Router {
    let (Some(identity), Some(debug_files), Some(signer)) = (identity, debug_files, signer) else {
        return Router::new();
    };
    let mut router = Router::new()
        .route(
            "/api/0/organizations/{organization_slug}/chunk-upload/",
            get(chunk_options).post(chunk_upload),
        )
        .route(
            "/api/0/projects/{organization_slug}/{project_slug}/files/difs/assemble/",
            post(assemble),
        )
        .route(
            "/api/v1/projects/{project_id}/debug-files/{file_id}",
            delete(delete_debug_file),
        )
        .route(
            "/internal/symbolicator/projects/{project_id}/debug-files/",
            get(private_source),
        );
    if artifacts.is_some() {
        router = router
            .route(
                "/api/0/organizations/{organization_slug}/artifactbundle/assemble/",
                post(assemble_artifact),
            )
            .route(
                "/internal/symbolicator/projects/{project_id}/artifact-lookup/",
                get(private_artifact),
            );
    }
    router.with_state(DebugHttpState {
        identity,
        debug_files,
        artifacts,
        signer,
        trusted_proxies: trusted_proxies.into(),
    })
}

async fn chunk_options(
    State(state): State<DebugHttpState>,
    Path(organization_slug): Path<String>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
) -> Result<Json<Value>, DebugHttpError> {
    let _ = authenticate_upload_token(&state, &headers).await?;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 255
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".:-[]".contains(&byte))
        })
        .ok_or(InvalidRequest)?;
    let scheme = request_scheme(
        connect_info.map(|Extension(ConnectInfo(peer))| peer),
        &headers,
        &state.trusted_proxies,
    );
    let upload_url =
        format!("{scheme}://{host}/api/0/organizations/{organization_slug}/chunk-upload/");
    let maximum_file_bytes = state.debug_files.maximum_file_bytes();
    Ok(Json(json!({
        "url": upload_url,
        "chunkSize": SENTRY_CLI_CHUNK_BYTES,
        "chunksPerRequest": SENTRY_CLI_CHUNKS_PER_REQUEST,
        "maxFileSize": maximum_file_bytes,
        "maxRequestSize": SENTRY_CLI_MAX_REQUEST_BYTES,
        "concurrency": 4,
        "hashAlgorithm": "sha1",
        // No transport codec on purpose. With gzip advertised getsentry/cli stores bundle
        // entries uncompressed and gzips the chunks instead; bundles are kept as uploaded,
        // so they would take about four times the space. Gzipped chunks are still accepted.
        "compression": [],
        "accept": if state.artifacts.is_some() {
            vec![
                "debug_files",
                "release_files",
                "pdbs",
                "portablepdbs",
                "artifact_bundles",
                "artifact_bundles_v2",
            ]
        } else {
            vec!["debug_files", "pdbs", "portablepdbs"]
        },
        "maxWait": 300,
    })))
}

/// Scheme the client used to reach Metric. Behind a TLS-terminating proxy the
/// request itself is plain HTTP, and sentry-cli refuses to send its token to a URL
/// whose scheme or host differs from its configured one, so `X-Forwarded-Proto`
/// from a trusted proxy wins.
fn request_scheme(
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    trusted_proxies: &[TrustedProxy],
) -> &'static str {
    let from_trusted_proxy = peer.is_some_and(|peer| {
        trusted_proxies
            .iter()
            .any(|proxy| proxy.contains(peer.ip()))
    });
    // The first value is the one the client used when several proxies append theirs.
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"));
    if from_trusted_proxy && forwarded_https {
        "https"
    } else {
        "http"
    }
}

async fn chunk_upload(
    State(state): State<DebugHttpState>,
    Path(_organization_slug): Path<String>,
    request: Request,
) -> Result<Json<Value>, DebugHttpError> {
    let context = authenticate_upload_token(&state, request.headers()).await?;
    for part in chunk_parts(request).await? {
        let checksum = checksum_from_part(&part.headers)?;
        let bytes = chunk_bytes(part)?;
        state
            .debug_files
            .upload_chunk(
                &context,
                context.organization_id,
                checksum,
                bytes.into_boxed_slice(),
            )
            .await
            .map_err(map_debug)?;
    }
    Ok(Json(json!({})))
}

/// Multipart parts of one chunk upload request.
async fn chunk_parts(request: Request) -> Result<Vec<MultipartPart>, DebugHttpError> {
    let boundary = multipart_boundary(request.headers())?;
    let body = to_bytes(request.into_body(), MAX_CHUNK_REQUEST_BYTES)
        .await
        .map_err(|_| InvalidRequest)?;
    let parts = multipart_parts(&body, &boundary)?;
    if parts.is_empty() || parts.len() > SENTRY_CLI_CHUNKS_PER_REQUEST {
        return Err(InvalidRequest);
    }
    Ok(parts)
}

#[derive(Deserialize)]
struct ArtifactAssembleRequest {
    checksum: String,
    chunks: Vec<String>,
    projects: Vec<String>,
    version: Option<String>,
    dist: Option<String>,
}

async fn assemble_artifact(
    State(state): State<DebugHttpState>,
    Path(organization_slug): Path<String>,
    request: Request,
) -> Result<Json<Value>, DebugHttpError> {
    let context = authenticate_token(&state, request.headers(), Permission::ArtifactWrite).await?;
    let body = to_bytes(request.into_body(), MAX_ASSEMBLE_BODY_BYTES)
        .await
        .map_err(|_| InvalidRequest)?;
    let request: ArtifactAssembleRequest =
        serde_json::from_slice(&body).map_err(|_| InvalidRequest)?;
    let artifacts = state
        .artifacts
        .as_ref()
        .ok_or(DebugHttpError::Unavailable)?;
    let result = artifacts
        .assemble(
            &context,
            &organization_slug,
            AssembleArtifact {
                sha1: sha1_bytes(&request.checksum)?,
                chunks: request
                    .chunks
                    .iter()
                    .map(|chunk| sha1_bytes(chunk))
                    .collect::<Result<_, _>>()?,
                project_slugs: request
                    .projects
                    .into_iter()
                    .map(String::into_boxed_str)
                    .collect(),
                release: request.version.map(String::into_boxed_str),
                dist: request.dist.map(String::into_boxed_str),
            },
        )
        .await
        .map_err(map_artifact)?;
    let response = match result {
        AssembleArtifactState::Missing { chunks } => json!({
            "state": "not_found",
            "missingChunks": chunks.into_iter().map(hex::encode).collect::<Vec<_>>(),
            "detail": null,
        }),
        AssembleArtifactState::Ok { .. } => json!({
            "state": "ok",
            "missingChunks": [],
            "detail": null,
        }),
        AssembleArtifactState::Error { code } => json!({
            "state": "error",
            "missingChunks": [],
            "detail": format!("artifact_error_{code}"),
        }),
    };
    Ok(Json(response))
}

async fn private_artifact(
    State(state): State<DebugHttpState>,
    Path(project_id): Path<String>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, DebugHttpError> {
    let project_id = parse_project_id(&project_id)?;
    let token = bearer(&headers).ok_or(Unauthorized)?;
    if !state.signer.verify_artifact(project_id, token) {
        return Err(Unauthorized);
    }
    let raw_query = raw_query.unwrap_or_default();
    if raw_query.len() > 16 * 1024 {
        return Err(InvalidRequest);
    }
    let mut revision = 0_u64;
    let mut debug_ids = Vec::new();
    let mut release = None;
    let mut dist = None;
    let mut id = None;
    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        match key.as_ref() {
            "revision" => revision = value.parse().map_err(|_| InvalidRequest)?,
            "debug_id" => debug_ids.push(DebugId::parse(&value).map_err(|_| InvalidRequest)?),
            "release" => release = Some(value.into_owned()),
            "dist" => dist = Some(value.into_owned()),
            "id" => id = Some(value.into_owned()),
            _ => {}
        }
    }
    let _revision = revision;
    let artifacts = state
        .artifacts
        .as_ref()
        .ok_or(DebugHttpError::Unavailable)?;
    if let Some(id) = id {
        let id = ArtifactBundleId::parse(&id).map_err(|_| InvalidRequest)?;
        let (bundle, reader) = artifacts.open(project_id, id).await.map_err(map_artifact)?;
        let stream = futures_util::stream::try_unfold(reader, |mut reader| async move {
            reader
                .read_chunk(64 * 1024)
                .await
                .map(|chunk| chunk.map(|bytes| (bytes::Bytes::from(bytes.into_vec()), reader)))
                .map_err(std::io::Error::other)
        });
        let mut response = Body::from_stream(stream).into_response();
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{}\"", hex::encode(bundle.checksum)))
                .map_err(|_| DebugHttpError::Unavailable)?,
        );
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&bundle.size.to_string())
                .map_err(|_| DebugHttpError::Unavailable)?,
        );
        return Ok(response);
    }
    if debug_ids.is_empty() && release.is_none() {
        return Err(InvalidRequest);
    }
    let candidates = artifacts
        .lookup(
            project_id,
            debug_ids,
            release.as_deref(),
            dist.map(String::into_boxed_str),
        )
        .await
        .map_err(map_artifact)?;
    Ok(Json(json!(
        candidates
            .into_iter()
            .map(|candidate| json!({
                "id": candidate.bundle.id.to_string(),
                "symbolType": "sourcebundle",
            }))
            .collect::<Vec<_>>()
    ))
    .into_response())
}

#[derive(Deserialize)]
struct AssembleEntry {
    name: String,
    debug_id: Option<String>,
    code_id: Option<String>,
    chunks: Vec<String>,
}

async fn assemble(
    State(state): State<DebugHttpState>,
    Path((organization_slug, project_slug)): Path<(String, String)>,
    request: Request,
) -> Result<Json<Value>, DebugHttpError> {
    let context = authenticate_token(&state, request.headers(), Permission::DebugFileWrite).await?;
    let body = to_bytes(request.into_body(), MAX_ASSEMBLE_BODY_BYTES)
        .await
        .map_err(|_| InvalidRequest)?;
    let entries: BTreeMap<String, AssembleEntry> =
        serde_json::from_slice(&body).map_err(|_| InvalidRequest)?;
    if entries.is_empty() || entries.len() > 64 {
        return Err(InvalidRequest);
    }
    let (organization_id, project) = state
        .debug_files
        .resolve_project(
            &context,
            &organization_slug,
            &project_slug,
            Permission::DebugFileWrite,
        )
        .await
        .map_err(map_debug)?;
    let mut response = serde_json::Map::new();
    for (sha1_text, entry) in entries {
        let sha1 = sha1_bytes(&sha1_text)?;
        let chunks = entry
            .chunks
            .iter()
            .map(|value| sha1_bytes(value))
            .collect::<Result<Vec<_>, _>>()?;
        let state_result = state
            .debug_files
            .assemble(
                &context,
                organization_id,
                project.id,
                AssembleDebugFile {
                    sha1,
                    name: entry.name.into_boxed_str(),
                    debug_id: entry
                        .debug_id
                        .as_deref()
                        .map(DebugId::parse)
                        .transpose()
                        .map_err(|_| InvalidRequest)?,
                    code_id: entry
                        .code_id
                        .as_deref()
                        .map(CodeId::parse)
                        .transpose()
                        .map_err(|_| InvalidRequest)?,
                    chunks,
                },
            )
            .await
            .map_err(map_debug)?;
        let value = match state_result {
            AssembleState::Missing { chunks } => json!({
                "state": "not_found",
                "missingChunks": chunks.into_iter().map(hex::encode).collect::<Vec<_>>(),
            }),
            AssembleState::Ok { file, .. } => json!({
                "state": "ok",
                "missingChunks": [],
                "detail": null,
                "dif": {
                    "id": file.id.to_string(),
                    "debugId": file.debug_id.map(|value| value.to_string()),
                    "codeId": file.code_id.map(|value| value.as_str().to_owned()),
                    "cpuName": "unknown",
                    "objectName": file.name,
                    "symbolType": file.file_type.symbolicator_name(),
                    "features": ["debug"],
                    "sha1": sha1_text,
                    "size": file.size,
                }
            }),
            AssembleState::Error { code } => json!({
                "state": "error",
                "missingChunks": [],
                "detail": code,
            }),
        };
        response.insert(sha1_text, value);
    }
    Ok(Json(Value::Object(response)))
}

async fn delete_debug_file(
    State(state): State<DebugHttpState>,
    Path((project_id, file_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, DebugHttpError> {
    let context = authenticate_token(&state, &headers, Permission::DebugFileDelete).await?;
    let project_id = parse_project_id(&project_id)?;
    let file_id = DebugFileId::parse(&file_id).map_err(|_| InvalidRequest)?;
    let deleted = state
        .debug_files
        .delete(&context, project_id, file_id)
        .await
        .map_err(map_debug)?;
    Ok(if deleted {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}

#[derive(Deserialize)]
struct PrivateQuery {
    revision: Option<u64>,
    debug_id: Option<String>,
    code_id: Option<String>,
    id: Option<String>,
}

async fn private_source(
    State(state): State<DebugHttpState>,
    Path(project_id): Path<String>,
    Query(query): Query<PrivateQuery>,
    headers: HeaderMap,
) -> Result<Response, DebugHttpError> {
    let project_id = parse_project_id(&project_id)?;
    verify_private_token(&state, &headers, project_id)?;
    let _revision = query.revision.unwrap_or(0);
    if let Some(file_id) = query.id {
        let file_id = DebugFileId::parse(&file_id).map_err(|_| InvalidRequest)?;
        let (file, reader) = state
            .debug_files
            .open(project_id, file_id)
            .await
            .map_err(map_debug)?;
        let stream = futures_util::stream::try_unfold(reader, |mut reader| async move {
            reader
                .read_chunk(64 * 1024)
                .await
                .map(|chunk| chunk.map(|bytes| (bytes::Bytes::from(bytes.into_vec()), reader)))
                .map_err(std::io::Error::other)
        });
        let mut response = Body::from_stream(stream).into_response();
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{}\"", hex::encode(file.checksum)))
                .map_err(|_| DebugHttpError::Unavailable)?,
        );
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&file.size.to_string())
                .map_err(|_| DebugHttpError::Unavailable)?,
        );
        return Ok(response);
    }
    let debug_id = query
        .debug_id
        .as_deref()
        .map(DebugId::parse)
        .transpose()
        .map_err(|_| InvalidRequest)?;
    let code_id = query
        .code_id
        .as_deref()
        .map(CodeId::parse)
        .transpose()
        .map_err(|_| InvalidRequest)?;
    if debug_id.is_none() && code_id.is_none() {
        return Err(InvalidRequest);
    }
    let files = state
        .debug_files
        .find(project_id, debug_id, code_id)
        .await
        .map_err(map_debug)?;
    Ok(Json(json!(
        files
            .into_iter()
            .map(|file| json!({
                "id": file.id.to_string(),
                "symbolType": file.file_type.symbolicator_name(),
            }))
            .collect::<Vec<_>>()
    ))
    .into_response())
}

async fn authenticate_token(
    state: &DebugHttpState,
    headers: &HeaderMap,
    permission: Permission,
) -> Result<AuthContext, DebugHttpError> {
    let token = bearer(headers).ok_or(Unauthorized)?;
    let context = state
        .identity
        .authenticate_api_token(&plain_secret(token)?)
        .await
        .map_err(|_| Unauthorized)?;
    context
        .permissions
        .contains(permission)
        .then_some(context)
        .ok_or(DebugHttpError::Forbidden)
}

async fn authenticate_upload_token(
    state: &DebugHttpState,
    headers: &HeaderMap,
) -> Result<AuthContext, DebugHttpError> {
    let token = bearer(headers).ok_or(Unauthorized)?;
    let context = state
        .identity
        .authenticate_api_token(&plain_secret(token)?)
        .await
        .map_err(|_| Unauthorized)?;
    (context.permissions.contains(Permission::DebugFileWrite)
        || context.permissions.contains(Permission::ArtifactWrite))
    .then_some(context)
    .ok_or(DebugHttpError::Forbidden)
}

fn verify_private_token(
    state: &DebugHttpState,
    headers: &HeaderMap,
    project_id: ProjectId,
) -> Result<(), DebugHttpError> {
    let token = bearer(headers).ok_or(Unauthorized)?;
    state
        .signer
        .verify(project_id, token)
        .then_some(())
        .ok_or(Unauthorized)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn plain_secret(value: &str) -> Result<PlainSecret, DebugHttpError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Unauthorized);
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| Unauthorized)?;
    Ok(PlainSecret::new(bytes))
}

fn multipart_boundary(headers: &HeaderMap) -> Result<Vec<u8>, DebugHttpError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(InvalidRequest)?;
    let boundary = content_type
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("boundary="))
        .map(|value| value.trim_matches('"'))
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 70
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&byte))
        })
        .ok_or(InvalidRequest)?;
    Ok(boundary.as_bytes().to_vec())
}

struct MultipartPart {
    headers: Vec<u8>,
    body: Vec<u8>,
}

fn multipart_parts(body: &[u8], boundary: &[u8]) -> Result<Vec<MultipartPart>, DebugHttpError> {
    let marker = [b"--".as_slice(), boundary].concat();
    let mut cursor = 0;
    let mut parts = Vec::new();
    while let Some(start) = find_bytes(&body[cursor..], &marker) {
        cursor += start + marker.len();
        if body.get(cursor..cursor + 2) == Some(b"--") {
            break;
        }
        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(InvalidRequest);
        }
        cursor += 2;
        let header_end = find_bytes(&body[cursor..], b"\r\n\r\n").ok_or(InvalidRequest)?;
        let headers = body[cursor..cursor + header_end].to_vec();
        cursor += header_end + 4;
        let next_marker = [b"\r\n--".as_slice(), boundary].concat();
        let data_end = find_bytes(&body[cursor..], &next_marker).ok_or(InvalidRequest)?;
        let data = body[cursor..cursor + data_end].to_vec();
        parts.push(MultipartPart {
            headers,
            body: data,
        });
        cursor += data_end + 2;
    }
    Ok(parts)
}

/// Chunk content of a multipart part: the client names gzipped chunks `file_gzip`
/// and plain ones `file`.
fn chunk_bytes(part: MultipartPart) -> Result<Vec<u8>, DebugHttpError> {
    let bytes = match field_name(&part.headers)? {
        "file" => part.body,
        "file_gzip" => {
            let mut bytes = Vec::with_capacity(SENTRY_CLI_CHUNK_BYTES);
            GzDecoder::new(part.body.as_slice())
                .take(SENTRY_CLI_CHUNK_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| InvalidRequest)?;
            bytes
        }
        _ => return Err(InvalidRequest),
    };
    if bytes.len() > SENTRY_CLI_CHUNK_BYTES {
        return Err(InvalidRequest);
    }
    Ok(bytes)
}

/// The `name` parameter of the part's `Content-Disposition` header.
fn field_name(headers: &[u8]) -> Result<&str, DebugHttpError> {
    let text = std::str::from_utf8(headers).map_err(|_| InvalidRequest)?;
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(header, _)| header.trim().eq_ignore_ascii_case("content-disposition"))
        .and_then(|(_, value)| {
            value.split(';').find_map(|parameter| {
                let (key, value) = parameter.split_once('=')?;
                key.trim()
                    .eq_ignore_ascii_case("name")
                    .then(|| value.trim().trim_matches('"'))
            })
        })
        .ok_or(InvalidRequest)
}

fn checksum_from_part(headers: &[u8]) -> Result<[u8; 20], DebugHttpError> {
    let text = std::str::from_utf8(headers).map_err(|_| InvalidRequest)?;
    text.split(['"', '=', ';', ' ', '\r', '\n'])
        .find(|token| token.len() == 40 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(sha1_bytes)
        .transpose()?
        .ok_or(InvalidRequest)
}

fn sha1_bytes(value: &str) -> Result<[u8; 20], DebugHttpError> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(InvalidRequest);
    }
    let mut bytes = [0_u8; 20];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| InvalidRequest)?;
    Ok(bytes)
}

fn parse_project_id(value: &str) -> Result<ProjectId, DebugHttpError> {
    value
        .parse()
        .ok()
        .and_then(|value| ProjectId::new(value).ok())
        .ok_or(InvalidRequest)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    (!needle.is_empty())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn map_debug(error: DebugFileError) -> DebugHttpError {
    match error {
        DebugFileError::InvalidRequest => DebugHttpError::InvalidRequest,
        DebugFileError::Forbidden => DebugHttpError::Forbidden,
        DebugFileError::NotFound => DebugHttpError::NotFound,
        DebugFileError::Quota => DebugHttpError::Quota,
        DebugFileError::Conflict => DebugHttpError::Conflict,
        DebugFileError::Unavailable => DebugHttpError::Unavailable,
    }
}

fn map_artifact(error: ArtifactError) -> DebugHttpError {
    match error {
        ArtifactError::InvalidRequest
        | ArtifactError::MalformedBundle
        | ArtifactError::ArchiveLimit
        | ArtifactError::UnsupportedCompression => DebugHttpError::InvalidRequest,
        ArtifactError::Forbidden => DebugHttpError::Forbidden,
        ArtifactError::NotFound => DebugHttpError::NotFound,
        ArtifactError::Quota => DebugHttpError::Quota,
        ArtifactError::Conflict | ArtifactError::Busy => DebugHttpError::Conflict,
        ArtifactError::Unavailable => DebugHttpError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn headers(proto: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(proto) = proto {
            headers.insert("x-forwarded-proto", HeaderValue::from_str(proto).unwrap());
        }
        headers
    }

    fn proxies() -> [TrustedProxy; 1] {
        [TrustedProxy::from_str("10.42.0.0/16").unwrap()]
    }

    #[test]
    fn upload_url_scheme_follows_trusted_proxy() {
        let proxy = Some(SocketAddr::from(([10, 42, 0, 7], 41_000)));
        assert_eq!(
            request_scheme(proxy, &headers(Some("https")), &proxies()),
            "https"
        );
        assert_eq!(
            request_scheme(proxy, &headers(Some("HTTPS")), &proxies()),
            "https"
        );
        assert_eq!(
            request_scheme(proxy, &headers(Some("https, http")), &proxies()),
            "https"
        );
        assert_eq!(
            request_scheme(proxy, &headers(Some("http")), &proxies()),
            "http"
        );
        assert_eq!(request_scheme(proxy, &headers(None), &proxies()), "http");
    }

    fn part(disposition: &str, body: &[u8]) -> MultipartPart {
        MultipartPart {
            headers: format!(
                "Content-Disposition: {disposition}\r\nContent-Type: application/octet-stream"
            )
            .into_bytes(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn chunk_is_plain_or_gzipped_by_field_name() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        let plain = part(
            &format!(r#"form-data; name="file"; filename="{sha1}""#),
            b"chunk",
        );
        assert_eq!(chunk_bytes(plain).unwrap(), b"chunk");

        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gzip, b"chunk").unwrap();
        let gzipped = part(
            &format!(r#"form-data; name="file_gzip"; filename="{sha1}""#),
            &gzip.finish().unwrap(),
        );
        assert_eq!(chunk_bytes(gzipped).unwrap(), b"chunk");
    }

    #[test]
    fn chunk_without_known_field_name_or_over_chunk_size_is_rejected() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        // `filename` is not the field name.
        let unnamed = part(&format!(r#"form-data; filename="{sha1}""#), b"chunk");
        assert!(chunk_bytes(unnamed).is_err());
        let foreign = part(r#"form-data; name="attachment""#, b"chunk");
        assert!(chunk_bytes(foreign).is_err());
        let oversized = part(
            r#"form-data; name="file""#,
            &vec![0; SENTRY_CLI_CHUNK_BYTES + 1],
        );
        assert!(chunk_bytes(oversized).is_err());
    }

    /// A chunk upload request carrying `chunks` full chunks.
    fn chunk_batch_request(chunks: usize) -> Request {
        let boundary = "metric-chunk-batch";
        let mut body = Vec::new();
        for index in 0..chunks {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                     filename=\"{index:040x}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            );
            body.resize(body.len() + SENTRY_CLI_CHUNK_BYTES, 7);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        axum::http::Request::builder()
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn full_chunk_batch_fits_with_its_multipart_framing() {
        // Clients fill maxRequestSize with chunk bytes alone.
        let chunks = SENTRY_CLI_MAX_REQUEST_BYTES / SENTRY_CLI_CHUNK_BYTES;
        let parts = chunk_parts(chunk_batch_request(chunks)).await.unwrap();
        assert_eq!(parts.len(), chunks);
        for part in parts {
            assert_eq!(chunk_bytes(part).unwrap().len(), SENTRY_CLI_CHUNK_BYTES);
        }
    }

    #[tokio::test]
    async fn chunk_batch_over_the_request_limit_is_rejected() {
        let chunks = SENTRY_CLI_MAX_REQUEST_BYTES / SENTRY_CLI_CHUNK_BYTES + 1;
        assert!(matches!(
            chunk_parts(chunk_batch_request(chunks)).await,
            Err(InvalidRequest)
        ));
    }

    #[test]
    fn upload_url_scheme_ignores_untrusted_forwarding() {
        let client = Some(SocketAddr::from(([203, 0, 113, 9], 41_000)));
        assert_eq!(
            request_scheme(client, &headers(Some("https")), &proxies()),
            "http"
        );
        assert_eq!(
            request_scheme(None, &headers(Some("https")), &proxies()),
            "http"
        );
    }
}
