//! HTTP API, versioned under `/api/v1`.
//!
//! Routes and their `OpenAPI` description are registered together, so
//! [`openapi`] always describes what [`router`] serves. The committed copy is
//! `schemas/api.openapi.json`; a test fails when it drifts.

use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures::{Stream, StreamExt as _, TryStreamExt as _};
use serde::{Deserialize, Serialize};
use tower_http::{
    cors::{AllowOrigin, Any, CorsLayer},
    set_header::SetResponseHeaderLayer,
};
use utoipa::{
    Modify, OpenApi, ToSchema,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouter},
    routes,
};

use crate::{
    auth::{AuthError, Authenticator},
    db::{AssetRecord, Job, JobState, JobStore, NewJob},
    index::{self, IndexError},
    previews,
    runner::{JobEvent, Runner},
    settings::Settings,
    storage::{Storage, StorageError, StoredObject},
    submission,
};

/// Largest accepted asset upload; the asset loader reads at most 64 MiB.
const ASSET_LIMIT: usize = 64 * 1024 * 1024;
/// Largest accepted job submission.
const JOB_LIMIT: usize = 1024 * 1024;
/// Path prefix of every API route.
const API_PREFIX: &str = "/api/v1";
/// Shortest and longest export link lifetime; S3 signatures last at most 7 days.
const EXPORT_EXPIRY_S: std::ops::RangeInclusive<u64> = 60..=604_800;

/// Handles the API shares.
#[derive(Clone)]
pub struct AppState {
    /// Job records.
    pub jobs: JobStore,
    /// Assets and results.
    pub storage: Storage,
    /// Job execution.
    pub runner: Runner,
    /// Request authentication.
    pub auth: Authenticator,
    /// Server settings.
    pub settings: Arc<Settings>,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "bop-server",
        description = "Job API for deterministic bin-picking dataset generation."
    ),
    modifiers(&BearerToken),
    components(schemas(OidcInfo)),
    security(("bearer" = [])),
    components(schemas(JobEvent)),
    tags(
        (name = "server", description = "Server identity and capabilities."),
        (name = "assets", description = "The shared asset library."),
        (name = "configs", description = "Run configurations."),
        (name = "jobs", description = "Generation jobs and their results."),
        (name = "exports", description = "Packed downloads of finished jobs.")
    )
)]
struct ApiDoc;

/// Declares the bearer token (`BOP_API_TOKEN` or an OIDC access token) as the
/// API's security scheme.
struct BearerToken;

impl Modify for BearerToken {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "bearer",
                SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
            );
    }
}

/// Routes that answer without credentials, so a client can learn how to sign in.
fn public() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(auth_info))
}

/// Every authenticated `/api/v1` route with its description, relative to the prefix.
fn api() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(server_info))
        .routes(routes!(list_assets))
        .routes(limit(
            routes!(get_asset, put_asset, delete_asset),
            ASSET_LIMIT,
        ))
        .routes(routes!(optimize_asset))
        .routes(limit(routes!(validate_config), JOB_LIMIT))
        .routes(limit(routes!(list_jobs, create_job), JOB_LIMIT))
        .routes(routes!(get_job))
        .routes(routes!(get_job_config))
        .routes(routes!(cancel_job))
        .routes(routes!(resume_job))
        .routes(routes!(job_events))
        .routes(routes!(list_job_files))
        .routes(routes!(get_job_file))
        .routes(routes!(get_job_preview))
        .routes(routes!(create_export))
        .routes(routes!(get_export))
}

fn limit(
    (schemas, paths, method_router): UtoipaMethodRouter<AppState>,
    bytes: usize,
) -> UtoipaMethodRouter<AppState> {
    (
        schemas,
        paths,
        method_router.layer(DefaultBodyLimit::max(bytes)),
    )
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    let (api, _) = api()
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .merge(public())
        .split_for_parts();
    let cors = cors(&state.settings.cors_origins);
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest(API_PREFIX, api)
        .with_state(state)
        // Stored files are user uploads: never let a browser sniff or render
        // them as a page of this origin.
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ));
    match cors {
        Some(layer) => router.layer(layer),
        None => router,
    }
}

/// The `OpenAPI` document for every route [`router`] serves below `/api/v1`.
#[must_use]
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = api().merge(public()).into_openapi();
    // Axum's `{*rest}` wildcards are plain `{rest}` parameters in OpenAPI.
    doc.paths.paths = std::mem::take(&mut doc.paths.paths)
        .into_iter()
        .map(|(path, item)| (format!("{API_PREFIX}{}", path.replace("{*", "{")), item))
        .collect();
    doc
}

fn cors(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let allow = if origins.iter().any(|origin| origin == "*") {
        AllowOrigin::from(Any)
    } else {
        AllowOrigin::list(
            origins
                .iter()
                .filter_map(|origin| HeaderValue::from_str(origin).ok()),
        )
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow)
            .allow_methods(Any)
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .expose_headers([header::CONTENT_DISPOSITION]),
    )
}

/// Requires a bearer token when the server has credentials configured.
async fn authorize(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    match state.auth.authenticate(bearer).await {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(AuthError::Forbidden) => error(StatusCode::FORBIDDEN, AuthError::Forbidden.to_string()),
        Err(AuthError::Keys(message)) => {
            tracing::error!("cannot authenticate: {message}");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "the sign-in provider is unreachable",
            )
        }
        Err(failure) => {
            tracing::debug!("rejected request: {failure}");
            let mut response = error(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            response
        }
    }
}

/// Why a request failed.
#[derive(Serialize, ToSchema)]
struct ApiError {
    /// Human-readable reason.
    error: String,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
        .into_response()
}

/// Logs `source` and answers without it: it can name hosts, buckets, or paths.
fn internal(source: impl std::fmt::Display) -> Response {
    tracing::error!("request failed: {source}");
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error; see the server log",
    )
}

fn storage_failure(source: StorageError) -> Response {
    match source {
        StorageError::Store(object_store::Error::NotFound { .. }) => {
            error(StatusCode::NOT_FOUND, "no such file")
        }
        other => internal(other),
    }
}

fn index_failure(source: IndexError) -> Response {
    match source {
        IndexError::Storage(source) => storage_failure(source),
        other => internal(other),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// How a browser may cache a stored object.
#[derive(Clone, Copy)]
enum Caching {
    /// It never changes once stored: keep it without asking again.
    Immutable,
    /// It may be replaced: keep it, but revalidate with its entity tag.
    Revalidate,
}

impl Caching {
    const fn header(self) -> &'static str {
        match self {
            Self::Immutable => "private, max-age=31536000, immutable",
            Self::Revalidate => "private, no-cache",
        }
    }
}

/// A store's entity tag as an HTTP `ETag`, which is always quoted.
fn http_etag(etag: &str) -> String {
    format!("\"{}\"", etag.trim_start_matches("W/").trim_matches('"'))
}

/// Whether an `If-None-Match` header names `etag`.
fn names_etag(if_none_match: &str, etag: &str) -> bool {
    let etag = http_etag(etag);
    if_none_match
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || http_etag(candidate) == etag)
}

/// Streams a stored object with a content type chosen by its extension,
/// answering 304 when the request's `If-None-Match` names its entity tag.
async fn stream_object(
    storage: &Storage,
    key: &str,
    disposition: Option<String>,
    caching: Caching,
    request: &HeaderMap,
) -> Response {
    let cache_control = (
        header::CACHE_CONTROL,
        HeaderValue::from_static(caching.header()),
    );
    if let Some(wanted) = request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        match storage.etag(key).await {
            Ok(Some(etag)) if names_etag(wanted, &etag) => {
                let mut response = (StatusCode::NOT_MODIFIED, [cache_control]).into_response();
                if let Ok(value) = HeaderValue::from_str(&http_etag(&etag)) {
                    response.headers_mut().insert(header::ETAG, value);
                }
                return response;
            }
            Ok(_) => {}
            Err(source) => return storage_failure(source),
        }
    }
    match storage.get(key).await {
        Ok(result) => {
            let content_type = match key.rsplit_once('.').map(|(_, extension)| extension) {
                Some("png") => "image/png",
                Some("json") => "application/json",
                Some("toml") => "application/toml",
                Some("stl") => "model/stl",
                Some("glb") => "model/gltf-binary",
                Some("gltf") => "model/gltf+json",
                Some("obj") => "model/obj",
                Some("tar") => "application/x-tar",
                _ => "application/octet-stream",
            };
            let etag = result
                .meta
                .e_tag
                .as_deref()
                .and_then(|etag| HeaderValue::from_str(&http_etag(etag)).ok());
            let body = Body::from_stream(result.into_stream().map_err(std::io::Error::other));
            let mut response = (
                [
                    (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
                    cache_control,
                ],
                body,
            )
                .into_response();
            if let Some(etag) = etag {
                response.headers_mut().insert(header::ETAG, etag);
            }
            if let Some(value) = disposition.and_then(|value| HeaderValue::from_str(&value).ok()) {
                response
                    .headers_mut()
                    .insert(header::CONTENT_DISPOSITION, value);
            }
            response
        }
        Err(source) => storage_failure(source),
    }
}

/// How to sign in to this server.
#[derive(Serialize, ToSchema)]
struct AuthInfo {
    /// Whether `/api` routes other than this one need a bearer token.
    required: bool,
    /// Whether a shared `BOP_API_TOKEN` is accepted.
    api_token: bool,
    /// The OIDC provider whose access tokens are accepted, if any.
    oidc: Option<OidcInfo>,
}

/// An OIDC provider a browser can sign in with (authorization code
/// with PKCE, as a public client).
#[derive(Serialize, ToSchema)]
struct OidcInfo {
    /// Issuer URL; its discovery document is at `.well-known/openid-configuration`.
    issuer: String,
    /// Client to sign in as.
    client_id: String,
    /// Space-separated scopes to request.
    scopes: String,
}

/// Reports which credentials the API accepts. Needs no credentials itself.
#[utoipa::path(get, path = "/auth", tag = "server", security(()),
    responses((status = 200, body = AuthInfo)))]
async fn auth_info(State(state): State<AppState>) -> Json<AuthInfo> {
    let settings = &state.settings;
    Json(AuthInfo {
        required: state.auth.required(),
        api_token: settings.api_token.is_some(),
        oidc: settings.oidc.as_ref().map(|oidc| OidcInfo {
            issuer: oidc.issuer.clone(),
            client_id: oidc.client_id.clone(),
            scopes: oidc.scopes.clone(),
        }),
    })
}

/// What this server is and can do.
#[derive(Serialize, ToSchema)]
struct ServerInfo {
    /// `bop-server` version.
    server: String,
    /// API version, the path segment after `/api/`.
    api: String,
    /// Renderers compiled into this build.
    renderers: Vec<String>,
    /// Largest `valid_scene_count` a job may request.
    max_scenes: u32,
    /// Whether export links are presigned storage URLs rather than API routes.
    signed_exports: bool,
}

/// Reports the server version, API version, and compiled renderers.
#[utoipa::path(get, path = "/version", tag = "server",
    responses((status = 200, body = ServerInfo)))]
async fn server_info(State(state): State<AppState>) -> Json<ServerInfo> {
    Json(ServerInfo {
        server: env!("CARGO_PKG_VERSION").to_owned(),
        api: API_PREFIX.trim_start_matches("/api/").to_owned(),
        renderers: ["wgpu", "blenderproc"]
            .into_iter()
            .filter(|name| submission::parse_renderer(name).is_ok())
            .map(str::to_owned)
            .collect(),
        max_scenes: state.settings.max_scenes,
        signed_exports: state.storage.can_sign(),
    })
}

/// Lists the asset library, sorted by path.
#[utoipa::path(get, path = "/assets", tag = "assets",
    responses((status = 200, body = Vec<StoredObject>), (status = 500, body = ApiError)))]
async fn list_assets(State(state): State<AppState>) -> Response {
    match state.jobs.assets().await {
        Ok(assets) => {
            let mut objects = assets
                .into_iter()
                .map(|asset| StoredObject {
                    path: asset.path,
                    size: asset.size,
                })
                .collect::<Vec<_>>();
            objects.sort_by(|left, right| left.path.cmp(&right.path));
            Json(objects).into_response()
        }
        Err(source) => internal(source),
    }
}

/// Downloads one asset file.
#[utoipa::path(get, path = "/assets/{*path}", tag = "assets",
    params(("path" = String, Path, description = "Asset path inside the library, e.g. `cup/cup.glb`.")),
    responses(
        (status = 200, description = "The file.", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn get_asset(
    State(state): State<AppState>,
    Path(path): Path<String>,
    request: HeaderMap,
) -> Response {
    let key = match submission::storage_key(std::path::Path::new(&path)) {
        Ok(key) => key,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    // An asset can be replaced under the same path.
    stream_object(
        &state.storage,
        &format!("assets/{key}"),
        None,
        Caching::Revalidate,
        &request,
    )
    .await
}

/// An uploaded asset's library path.
#[derive(Serialize, ToSchema)]
struct UploadedAsset {
    /// Path inside the library, as configurations reference it.
    path: String,
}

/// Uploads or replaces one asset file (at most 64 MiB).
#[utoipa::path(put, path = "/assets/{*path}", tag = "assets",
    params(("path" = String, Path, description = "Asset path inside the library, e.g. `cup/cup.glb`.")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = UploadedAsset), (status = 400, body = ApiError)))]
async fn put_asset(
    State(state): State<AppState>,
    Path(path): Path<String>,
    body: Bytes,
) -> Response {
    let key = match submission::storage_key(std::path::Path::new(&path)) {
        Ok(key) => key,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let etag = match state
        .storage
        .put(&format!("assets/{key}"), body.to_vec())
        .await
    {
        Ok(etag) => etag,
        Err(source) => return storage_failure(source),
    };
    let record = AssetRecord {
        path: key,
        size: body.len() as u64,
        etag,
    };
    match state.jobs.upsert_asset(&record).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(UploadedAsset { path: record.path }),
        )
            .into_response(),
        Err(source) => internal(source),
    }
}

/// Deletes one asset file.
#[utoipa::path(delete, path = "/assets/{*path}", tag = "assets",
    params(("path" = String, Path, description = "Asset path inside the library.")),
    responses((status = 204), (status = 400, body = ApiError)))]
async fn delete_asset(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    let key = match submission::storage_key(std::path::Path::new(&path)) {
        Ok(key) => key,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    // Unindexed first: a failure in between leaves an object the next start indexes again.
    if let Err(source) = state.jobs.delete_asset(&key).await {
        return internal(source);
    }
    match state.storage.delete(&format!("assets/{key}")).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(source) => storage_failure(source),
    }
}

const fn default_max_deviation_mm() -> f32 {
    0.1
}

const fn default_crease_angle_deg() -> f32 {
    30.0
}

/// A library file to optimize.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct OptimizeAsset {
    /// Source path inside the library: an `.stl` or `.glb` file.
    path: String,
    /// Largest allowed surface deviation in millimetres; 0 only welds
    /// duplicate vertices. Default 0.1.
    #[serde(default = "default_max_deviation_mm")]
    max_deviation_mm: f32,
    /// Faces meeting at a sharper angle keep separate normals. Default 30.
    #[serde(default = "default_crease_angle_deg")]
    crease_angle_deg: f32,
    /// Metres per STL unit, e.g. 0.001 for millimetres. Required for STL;
    /// GLB is always metres.
    source_unit_scale_m: Option<f32>,
}

/// The optimized asset and what changed.
#[derive(Serialize, ToSchema)]
struct OptimizedAssetReport {
    /// Path of the written GLB, next to the source as `<name>.optimized.glb`.
    /// It is in metres, so configurations use `source_unit_scale_m = 1.0`.
    path: String,
    /// Source file size in bytes.
    source_bytes: u64,
    /// Optimized file size in bytes.
    bytes: u64,
    /// Source triangle count.
    source_triangles: u64,
    /// Optimized triangle count.
    triangles: u64,
    /// Source vertex count.
    source_vertices: u64,
    /// Optimized vertex count.
    vertices: u64,
    /// Largest measured distance between the source and optimized surfaces, in millimetres.
    max_deviation_mm: f32,
}

/// The library path an optimized `source` is written to.
fn optimized_path(source: &str) -> Option<String> {
    let (stem, extension) = source.rsplit_once('.')?;
    let supported = extension.eq_ignore_ascii_case("glb") || extension.eq_ignore_ascii_case("stl");
    (supported && !stem.ends_with(".optimized")).then(|| format!("{stem}.optimized.glb"))
}

/// Welds and simplifies a library mesh within a deviation budget, and stores
/// the result next to it. An earlier result for the same source is replaced.
#[utoipa::path(post, path = "/asset-optimizations", tag = "assets",
    request_body = OptimizeAsset,
    responses(
        (status = 201, body = OptimizedAssetReport),
        (status = 400, body = ApiError), (status = 404, body = ApiError), (status = 422, body = ApiError)))]
async fn optimize_asset(
    State(state): State<AppState>,
    Json(request): Json<OptimizeAsset>,
) -> Response {
    let key = match submission::storage_key(std::path::Path::new(&request.path)) {
        Ok(key) => key,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let Some(output) = optimized_path(&key) else {
        return error(
            StatusCode::BAD_REQUEST,
            "only .stl and .glb files that are not already optimized can be optimized",
        );
    };
    let is_stl = key.to_ascii_lowercase().ends_with(".stl");
    let unit_scale = match (is_stl, request.source_unit_scale_m) {
        (true, None) => {
            return error(
                StatusCode::BAD_REQUEST,
                "STL has no units: source_unit_scale_m is required",
            );
        }
        (_, scale) => scale.unwrap_or(1.0),
    };
    let options = match bop_pipeline::MeshOptimizationOptions::try_new(
        request.max_deviation_mm / 1000.0,
        request.crease_angle_deg,
    ) {
        Ok(options) => options,
        Err(source) => return error(StatusCode::BAD_REQUEST, source.to_string()),
    };
    let bytes = match state.storage.get(&format!("assets/{key}")).await {
        Ok(result) => match result.bytes().await {
            Ok(bytes) => bytes,
            Err(source) => return storage_failure(source.into()),
        },
        Err(source) => return storage_failure(source),
    };
    let source_bytes = bytes.len();
    let file_name = std::path::PathBuf::from(&key);
    let optimized = tokio::task::spawn_blocking(move || {
        bop_pipeline::optimize_asset_source(&file_name, &bytes, unit_scale, options)
    })
    .await;
    let optimized = match optimized {
        Ok(Ok(optimized)) => optimized,
        Ok(Err(source)) => return error(StatusCode::UNPROCESSABLE_ENTITY, source.to_string()),
        Err(source) => return internal(source),
    };
    let report = optimized.report;
    let size = optimized.glb.len();
    let etag = match state
        .storage
        .put(&format!("assets/{output}"), optimized.glb)
        .await
    {
        Ok(etag) => etag,
        Err(source) => return storage_failure(source),
    };
    let record = AssetRecord {
        path: output.clone(),
        size: size as u64,
        etag,
    };
    if let Err(source) = state.jobs.upsert_asset(&record).await {
        return internal(source);
    }
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    (
        StatusCode::CREATED,
        Json(OptimizedAssetReport {
            path: output,
            source_bytes: count(source_bytes),
            bytes: count(size),
            source_triangles: count(report.source_triangles),
            triangles: count(report.triangles),
            source_vertices: count(report.source_vertices),
            vertices: count(report.vertices),
            max_deviation_mm: report.max_deviation_m * 1000.0,
        }),
    )
        .into_response()
}

/// Asset paths the run reads that are not in the library.
async fn missing_assets(jobs: &JobStore, paths: &[String]) -> Result<Vec<String>, Response> {
    let present = jobs.present_assets(paths).await.map_err(internal)?;
    Ok(paths
        .iter()
        .filter(|path| !present.contains(*path))
        .cloned()
        .collect())
}

/// A configuration to check.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ValidateConfig {
    /// Run configuration TOML; asset paths are relative to the asset library.
    config: String,
}

/// What a valid configuration would run.
#[derive(Serialize, ToSchema)]
struct ConfigReport {
    /// Requested scene count.
    total_scenes: u32,
    /// Asset library paths the run reads.
    asset_paths: Vec<String>,
    /// Of those, the ones not uploaded yet; a job is refused until this is empty.
    missing_assets: Vec<String>,
}

/// Checks a configuration exactly as job submission would, without queuing a job.
#[utoipa::path(post, path = "/configs/validate", tag = "configs",
    request_body = ValidateConfig,
    responses((status = 200, body = ConfigReport), (status = 422, body = ApiError)))]
async fn validate_config(
    State(state): State<AppState>,
    Json(request): Json<ValidateConfig>,
) -> Response {
    let accepted = match submission::accept(&request.config, &state.settings) {
        Ok(accepted) => accepted,
        Err(message) => return error(StatusCode::UNPROCESSABLE_ENTITY, message),
    };
    match missing_assets(&state.jobs, &accepted.asset_paths).await {
        Ok(missing) => Json(ConfigReport {
            total_scenes: accepted.total_scenes,
            asset_paths: accepted.asset_paths,
            missing_assets: missing,
        })
        .into_response(),
        Err(response) => response,
    }
}

/// A job submission.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateJob {
    /// Run configuration TOML; asset paths are relative to the asset library.
    config: String,
    /// `wgpu` or `blenderproc`.
    renderer: String,
    /// Also store a BOP export when the dataset is complete. Default true.
    #[serde(default = "default_export_bop")]
    export_bop: bool,
}

const fn default_export_bop() -> bool {
    true
}

/// Queues a job.
#[utoipa::path(post, path = "/jobs", tag = "jobs",
    request_body = CreateJob,
    responses((status = 201, body = Job), (status = 400, body = ApiError), (status = 422, body = ApiError)))]
async fn create_job(State(state): State<AppState>, Json(request): Json<CreateJob>) -> Response {
    if let Err(message) = submission::parse_renderer(&request.renderer) {
        return error(StatusCode::BAD_REQUEST, message);
    }
    let accepted = match submission::accept(&request.config, &state.settings) {
        Ok(accepted) => accepted,
        Err(message) => return error(StatusCode::UNPROCESSABLE_ENTITY, message),
    };
    match missing_assets(&state.jobs, &accepted.asset_paths).await {
        Ok(missing) => {
            if let Some(path) = missing.first() {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("asset {path} has not been uploaded"),
                );
            }
        }
        Err(response) => return response,
    }
    let id = uuid::Uuid::new_v4().to_string();
    let inserted = state
        .jobs
        .insert(&NewJob {
            id: &id,
            renderer: &request.renderer,
            export_bop: request.export_bop,
            // The submitted text is kept; the runner re-applies the server rewrite.
            config_toml: &request.config,
            total_scenes: i64::from(accepted.total_scenes),
        })
        .await;
    match inserted {
        Ok(job) => {
            state.runner.announce(&job);
            state.runner.wake();
            (StatusCode::CREATED, Json(job)).into_response()
        }
        Err(source) => internal(source),
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct ListQuery {
    /// Most jobs to return, newest first; 1 to 1000, default 100.
    limit: Option<i64>,
}

/// Lists jobs, newest first.
#[utoipa::path(get, path = "/jobs", tag = "jobs", params(ListQuery),
    responses((status = 200, body = Vec<Job>)))]
async fn list_jobs(State(state): State<AppState>, Query(query): Query<ListQuery>) -> Response {
    match state
        .jobs
        .list(query.limit.unwrap_or(100).clamp(1, 1000))
        .await
    {
        Ok(jobs) => Json(
            jobs.into_iter()
                .map(|job| state.runner.describe(job))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(source) => internal(source),
    }
}

/// Reports one job.
#[utoipa::path(get, path = "/jobs/{id}", tag = "jobs",
    params(("id" = String, Path, description = "Job identifier.")),
    responses((status = 200, body = Job), (status = 404, body = ApiError)))]
async fn get_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.jobs.get(&id).await {
        Ok(Some(job)) => Json(state.runner.describe(job)).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such job"),
        Err(source) => internal(source),
    }
}

/// Returns the configuration TOML the job was submitted with.
#[utoipa::path(get, path = "/jobs/{id}/config", tag = "jobs",
    params(("id" = String, Path, description = "Job identifier.")),
    responses((status = 200, content_type = "application/toml", body = String), (status = 404, body = ApiError)))]
async fn get_job_config(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.jobs.config(&id).await {
        Ok(Some(config)) => (
            [(header::CONTENT_TYPE, "application/toml; charset=utf-8")],
            config,
        )
            .into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "no such job"),
        Err(source) => internal(source),
    }
}

/// Cancels a queued job at once, or a running one after its in-flight scenes commit.
#[utoipa::path(post, path = "/jobs/{id}/cancel", tag = "jobs",
    params(("id" = String, Path, description = "Job identifier.")),
    responses(
        (status = 204, description = "A queued job was cancelled."),
        (status = 202, description = "The running job will stop."),
        (status = 409, body = ApiError)))]
async fn cancel_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.jobs.cancel_queued(&id).await {
        Ok(true) => {
            if let Ok(Some(job)) = state.jobs.get(&id).await {
                state.runner.announce(&job);
            }
            return StatusCode::NO_CONTENT.into_response();
        }
        Ok(false) => {}
        Err(source) => return internal(source),
    }
    // The server running the job hears of the request through the database.
    match state.jobs.request_cancel(&id).await {
        Ok(true) => {
            state.runner.cancel(&id);
            // The job stops after its in-flight scenes commit; tell watchers now.
            if let Ok(Some(job)) = state.jobs.get(&id).await {
                state.runner.announce(&job);
            }
            StatusCode::ACCEPTED.into_response()
        }
        Ok(false) => error(StatusCode::CONFLICT, "job is neither queued nor running"),
        Err(source) => internal(source),
    }
}

/// Queues a failed or cancelled job again; it continues from its committed scenes.
#[utoipa::path(post, path = "/jobs/{id}/resume", tag = "jobs",
    params(("id" = String, Path, description = "Job identifier.")),
    responses((status = 202), (status = 409, body = ApiError)))]
async fn resume_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.jobs.requeue(&id).await {
        Ok(true) => {
            if let Ok(Some(job)) = state.jobs.get(&id).await {
                state.runner.announce(&job);
            }
            state.runner.wake();
            StatusCode::ACCEPTED.into_response()
        }
        Ok(false) => error(StatusCode::CONFLICT, "only failed or cancelled jobs resume"),
        Err(source) => internal(source),
    }
}

/// Streams the job's current state, then every change, as server-sent `job` events.
///
/// The first event carries a full `Job`; later ones a `JobEvent`.
#[utoipa::path(get, path = "/jobs/{id}/events", tag = "jobs",
    params(("id" = String, Path, description = "Job identifier.")),
    responses(
        (status = 200, description = "Server-sent events named `job`.", content_type = "text/event-stream", body = JobEvent),
        (status = 404, body = ApiError)))]
async fn job_events(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    // Subscribe before reading the snapshot so no change falls in between.
    let receiver = state.runner.subscribe();
    let job = match state.jobs.get(&id).await {
        Ok(Some(job)) => job,
        Ok(None) => return error(StatusCode::NOT_FOUND, "no such job"),
        Err(source) => return internal(source),
    };
    let first = Event::default()
        .event("job")
        .json_data(state.runner.describe(job))
        .ok();
    let updates = futures::stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(event) => return Some((event, receiver)),
                // A slow client only misses intermediate progress.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .filter(move |event| std::future::ready(event.job_id == id))
    .filter_map(|event| std::future::ready(Event::default().event("job").json_data(&event).ok()));
    let stream: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(futures::stream::iter(first).chain(updates).map(Ok));
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

/// Lists the job's stored result files.
#[utoipa::path(get, path = "/jobs/{id}/files", tag = "jobs",
    params(("id" = String, Path, description = "Job identifier.")),
    responses((status = 200, body = Vec<StoredObject>), (status = 400, body = ApiError)))]
async fn list_job_files(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if let Err(message) = submission::storage_key(std::path::Path::new(&id)) {
        return error(StatusCode::BAD_REQUEST, message);
    }
    match index::job_files(&state.jobs, &state.storage, &id).await {
        Ok(objects) => Json(objects).into_response(),
        Err(source) => index_failure(source),
    }
}

/// Downloads one result file.
#[utoipa::path(get, path = "/jobs/{id}/files/{*path}", tag = "jobs",
    params(
        ("id" = String, Path, description = "Job identifier."),
        ("path" = String, Path, description = "File path as `/files` lists it.")),
    responses(
        (status = 200, description = "The file.", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 400, body = ApiError), (status = 404, body = ApiError)))]
async fn get_job_file(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    request: HeaderMap,
) -> Response {
    let key = match submission::storage_key(&std::path::Path::new(&id).join(&path)) {
        Ok(key) => key,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    // Committed scenes are written once; other files may be written again by
    // a later attempt.
    let caching = if path.starts_with("scenes/") {
        Caching::Immutable
    } else {
        Caching::Revalidate
    };
    stream_object(
        &state.storage,
        &format!("datasets/{key}"),
        None,
        caching,
        &request,
    )
    .await
}

/// The size of a preview.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct PreviewQuery {
    /// Longest edge in pixels, 16 to 4096; default 512. A smaller frame keeps its size.
    size: Option<u32>,
}

/// Downloads a small image of a stored frame: its `rgb.png`, `depth.exr`, or
/// `instance.png` scaled to fit `size`, with depth and instance IDs coloured
/// as the studio colours them. RGB and depth previews are JPEG, instance
/// previews PNG.
#[utoipa::path(get, path = "/jobs/{id}/previews/{*path}", tag = "jobs",
    params(
        ("id" = String, Path, description = "Job identifier."),
        ("path" = String, Path, description = "The frame file's path as `/files` lists it."),
        PreviewQuery),
    responses(
        (status = 200, description = "The preview.", content_type = "image/jpeg", body = Vec<u8>),
        (status = 400, body = ApiError), (status = 404, body = ApiError), (status = 422, body = ApiError)))]
async fn get_job_preview(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    Query(query): Query<PreviewQuery>,
) -> Response {
    let key = match submission::storage_key(&std::path::Path::new(&id).join(&path)) {
        Ok(key) => key,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let Some(file) = previews::FrameFile::of(&path) else {
        return error(
            StatusCode::BAD_REQUEST,
            "previews exist for a scene frame's rgb.png, depth.exr, or instance.png",
        );
    };
    let size = query.size.unwrap_or(previews::DEFAULT_SIZE);
    if !previews::SIZES.contains(&size) {
        return error(
            StatusCode::BAD_REQUEST,
            format!(
                "size must be between {} and {}",
                previews::SIZES.start(),
                previews::SIZES.end()
            ),
        );
    }
    let stored = match state.storage.get(&format!("datasets/{key}")).await {
        Ok(result) => match result.bytes().await {
            Ok(bytes) => bytes,
            Err(source) => return storage_failure(source.into()),
        },
        Err(source) => return storage_failure(source),
    };
    match tokio::task::spawn_blocking(move || previews::render(file, &stored, size)).await {
        // The frame never changes; a day bounds how long a changed preview
        // style takes to show.
        Ok(Ok(preview)) => (
            [
                (header::CONTENT_TYPE, preview.content_type),
                (header::CACHE_CONTROL, "private, max-age=86400"),
            ],
            preview.bytes,
        )
            .into_response(),
        Ok(Err(source)) => error(StatusCode::UNPROCESSABLE_ENTITY, source.to_string()),
        Err(source) => internal(source),
    }
}

/// What an export archive holds.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum ExportFormat {
    /// The dataset as generated.
    Dataset,
    /// The BOP export; the job must have been submitted with `export_bop`.
    Bop,
}

impl ExportFormat {
    const fn name(self) -> &'static str {
        match self {
            Self::Dataset => "dataset",
            Self::Bop => "bop",
        }
    }

    fn from_file(file: &str) -> Option<Self> {
        match file {
            "dataset.tar" => Some(Self::Dataset),
            "bop.tar" => Some(Self::Bop),
            _ => None,
        }
    }
}

const fn default_expiry() -> u64 {
    86_400
}

/// An export request.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateExport {
    /// What to pack.
    format: ExportFormat,
    /// Link lifetime in seconds, 60 to 604800 (7 days). Default 86400.
    #[serde(default = "default_expiry")]
    expires_in_s: u64,
}

/// A download link for a packed export.
#[derive(Serialize, ToSchema)]
struct ExportLink {
    /// What the archive holds.
    format: ExportFormat,
    /// Presigned storage URL, or an API path (relative to the server) when the
    /// storage cannot sign.
    url: String,
    /// Whether `url` is a presigned storage URL that needs no credentials.
    signed: bool,
    /// When a signed `url` stops working.
    expires_at_ms: Option<i64>,
    /// Archive size in bytes.
    size: u64,
    /// Suggested file name.
    file_name: String,
}

/// The finished job `id`, or the response explaining why there is none.
async fn succeeded_job(state: &AppState, id: &str) -> Result<Job, Response> {
    if let Err(message) = submission::storage_key(std::path::Path::new(id)) {
        return Err(error(StatusCode::BAD_REQUEST, message));
    }
    match state.jobs.get(id).await {
        Ok(Some(job)) if job.state == JobState::Succeeded => Ok(job),
        Ok(Some(_)) => Err(error(
            StatusCode::CONFLICT,
            "only succeeded jobs can be exported",
        )),
        Ok(None) => Err(error(StatusCode::NOT_FOUND, "no such job")),
        Err(source) => Err(internal(source)),
    }
}

/// Packs a succeeded job into an uncompressed tar (once; later requests reuse
/// it) and returns a download link.
// Jobs pack theirs as they finish; for older jobs the first request packs it,
// and concurrent requests wait for that one and share its archive.
#[utoipa::path(post, path = "/jobs/{id}/exports", tag = "exports",
    params(("id" = String, Path, description = "Job identifier.")),
    request_body = CreateExport,
    responses(
        (status = 200, body = ExportLink),
        (status = 400, body = ApiError), (status = 404, body = ApiError), (status = 409, body = ApiError)))]
async fn create_export(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<CreateExport>,
) -> Response {
    if !EXPORT_EXPIRY_S.contains(&request.expires_in_s) {
        return error(
            StatusCode::BAD_REQUEST,
            format!(
                "expires_in_s must be between {} and {}",
                EXPORT_EXPIRY_S.start(),
                EXPORT_EXPIRY_S.end()
            ),
        );
    }
    let job = match succeeded_job(&state, &id).await {
        Ok(job) => job,
        Err(response) => return response,
    };
    let format = request.format;
    if matches!(format, ExportFormat::Bop) && !job.export_bop {
        return error(StatusCode::CONFLICT, "job was submitted without export_bop");
    }
    let key = index::export_key(&id, format.name());
    let file_name = format!("{}.tar", index::export_root(&id, format.name()));
    let pack = {
        let storage = state.storage.clone();
        let (id, key) = (id.clone(), key.clone());
        let root = index::export_root(&id, format.name());
        // Jobs stored before exports were packed at finalize; newer ones are ready.
        move || async move {
            match format {
                ExportFormat::Dataset => {
                    storage
                        .pack_tar(
                            &format!("datasets/{id}"),
                            |path| !path.starts_with("bop/"),
                            &root,
                            &key,
                        )
                        .await
                }
                ExportFormat::Bop => {
                    storage
                        .pack_tar(&format!("datasets/{id}/bop"), |_| true, &root, &key)
                        .await
                }
            }
        }
    };
    let size =
        match index::ensure_export(&state.jobs, &state.storage, &id, format.name(), pack).await {
            Ok(size) => size,
            Err(source) => return index_failure(source),
        };
    let expires_in = Duration::from_secs(request.expires_in_s);
    let link = match state.storage.signed_get_url(&key, expires_in).await {
        Ok(Some(url)) => ExportLink {
            format,
            url,
            signed: true,
            expires_at_ms: Some(
                now_ms().saturating_add(i64::try_from(expires_in.as_millis()).unwrap_or(i64::MAX)),
            ),
            size,
            file_name,
        },
        Ok(None) => ExportLink {
            format,
            url: format!("{API_PREFIX}/jobs/{id}/exports/{}.tar", format.name()),
            signed: false,
            expires_at_ms: None,
            size,
            file_name,
        },
        Err(source) => return storage_failure(source),
    };
    Json(link).into_response()
}

/// Downloads a packed export through the API, for storage that cannot sign URLs.
#[utoipa::path(get, path = "/jobs/{id}/exports/{file}", tag = "exports",
    params(
        ("id" = String, Path, description = "Job identifier."),
        ("file" = String, Path, description = "`dataset.tar` or `bop.tar`.")),
    responses(
        (status = 200, description = "The archive.", content_type = "application/x-tar", body = Vec<u8>),
        (status = 404, body = ApiError)))]
async fn get_export(
    State(state): State<AppState>,
    Path((id, file)): Path<(String, String)>,
    request: HeaderMap,
) -> Response {
    let Some(format) = ExportFormat::from_file(&file) else {
        return error(StatusCode::NOT_FOUND, "no such export");
    };
    if let Err(response) = succeeded_job(&state, &id).await {
        return response;
    }
    stream_object(
        &state.storage,
        &format!("exports/{id}/{}.tar", format.name()),
        Some(format!(
            "attachment; filename=\"{}-{id}.tar\"",
            format.name()
        )),
        Caching::Revalidate,
        &request,
    )
    .await
}
