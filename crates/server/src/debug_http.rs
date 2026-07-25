//! Sentry CLI DIF upload and private Symbolicator source HTTP adapters.

use std::{collections::BTreeMap, io::Read, sync::Arc};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Query, RawQuery, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use metric_application::{
    artifacts::{ArtifactError, ArtifactService, AssembleArtifact, AssembleArtifactState},
    auth::IdentityService,
    debug_files::{AssembleDebugFile, AssembleState, DebugFileError, DebugFileService},
};
use metric_domain::{
    ProjectId,
    artifacts::ArtifactBundleId,
    auth::{AuthContext, Permission, PlainSecret},
    debug_files::{CodeId, DebugFileId, DebugId},
};
use metric_symbolication::PrivateSourceSigner;
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::debug_http::DebugHttpError::{InvalidRequest, Unauthorized};

const MAX_ASSEMBLE_BODY_BYTES: usize = 1024 * 1024;
const MAX_CHUNK_REQUEST_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct DebugHttpState {
    identity: Arc<IdentityService>,
    debug_files: Arc<DebugFileService>,
    artifacts: Option<Arc<ArtifactService>>,
    signer: PrivateSourceSigner,
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
    })
}

async fn chunk_options(
    State(state): State<DebugHttpState>,
    Path(organization_slug): Path<String>,
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
    let upload_url = format!("http://{host}/api/0/organizations/{organization_slug}/chunk-upload/");
    let maximum_file_bytes = state.debug_files.maximum_file_bytes();
    Ok(Json(json!({
        "url": upload_url,
        "chunkSize": 8 * 1024 * 1024,
        "chunksPerRequest": 64,
        "maxFileSize": maximum_file_bytes,
        "maxRequestSize": 32 * 1024 * 1024,
        "concurrency": 4,
        "hashAlgorithm": "sha1",
        "compression": ["gzip"],
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

async fn chunk_upload(
    State(state): State<DebugHttpState>,
    Path(_organization_slug): Path<String>,
    request: Request,
) -> Result<Json<Value>, DebugHttpError> {
    let context = authenticate_upload_token(&state, request.headers()).await?;
    let boundary = multipart_boundary(request.headers())?;
    let body = to_bytes(request.into_body(), MAX_CHUNK_REQUEST_BYTES)
        .await
        .map_err(|_| InvalidRequest)?;
    let parts = multipart_parts(&body, &boundary)?;
    if parts.is_empty() || parts.len() > 64 {
        return Err(InvalidRequest);
    }
    for part in parts {
        let checksum = checksum_from_part(&part.headers)?;
        let mut decoder = GzDecoder::new(part.body.as_slice());
        let mut bytes = Vec::with_capacity(8 * 1024 * 1024);
        decoder
            .by_ref()
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| InvalidRequest)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(InvalidRequest);
        }
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
