#![forbid(unsafe_code)]
//! Gateway route table + cross-cutting middleware (spec §11.1, §11.2, §12.3).
//!
//! The gateway is a thin, hardened reverse proxy in front of `core-api`. It:
//! - validates payloads against their schema BEFORE routing (spec §11.1/§12.3),
//! - authenticates + authorizes the principal (spec §12.6) via [`crate::auth`],
//! - rate-limits per IP and per key (spec §12.3) via [`crate::ratelimit`],
//! - injects/propagates the `correlation_id` (spec §13.1), and
//! - proxies the surviving request to the upstream `core-api`, returning the
//!   standard error envelope (spec §11.3) on any failure.
//!
//! Route → scope mapping mirrors spec §11.2 exactly.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Json;
use axum::Router;
use diyrag_common::correlation::{CorrelationId, HEADER_NAME};
use diyrag_common::errors::AppError;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

use crate::auth::{Authenticated, Requirement};
use crate::ratelimit::{self, RatePolicy};
use crate::GatewayState;

/// Local handler error newtype.
///
/// [`diyrag_common::errors::AppError`] is a foreign type, so the orphan rule
/// forbids `impl IntoResponse for AppError` in this crate. Handlers therefore
/// return `Result<Response, GatewayError>`; `?` converts any `AppError` into
/// this wrapper, and [`IntoResponse`] renders the standard envelope (spec §11.3).
pub struct GatewayError(pub AppError);

impl From<AppError> for GatewayError {
    fn from(e: AppError) -> Self {
        GatewayError(e)
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let err = self.0;
        let status =
            StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // TODO: write the error_log row (common/core-api) and use its log_id as
        //       the reference_code so the GUI deep-link resolves (spec §10.4/§13.2).
        let envelope = err.to_envelope("ERR-PENDING", CorrelationId::new());
        (status, Json(envelope)).into_response()
    }
}

/// Build the versioned `/api/v1` router with the full middleware stack.
///
/// Mounted under `/api/v1` by `main::build_app`. Each route declares its scope
/// (spec §11.2) and proxies to the matching `core-api` path. State is bound here
/// (`.with_state`), so the returned router is stateless (`Router<()>`) and nests
/// cleanly into the outer app.
#[must_use]
pub fn router(state: GatewayState) -> Router {
    // --- Read / query surface (scope: reader) ---
    let read_routes = Router::new()
        .route("/query/search", post(proxy_search))
        .route("/query/answer", post(proxy_answer))
        .route("/documents", get(proxy_passthrough))
        .route("/documents/{id}", get(proxy_passthrough))
        .route("/batch/{job_id}/status", get(proxy_passthrough))
        .route("/errors", get(proxy_passthrough));

    // --- Ingest surface (scope: ingest) ---
    let ingest_routes = Router::new()
        .route("/files/roots", post(proxy_passthrough))
        .route("/ingestion/trigger", post(proxy_passthrough))
        .route("/batch/submit", post(proxy_passthrough))
        // Stricter per-IP limit on the ingest path (spec §12.3 / §22 #7).
        .layer(ratelimit::per_ip_layer(RatePolicy::INGEST));

    // --- Admin surface (scope: admin) ---
    let admin_routes = Router::new()
        .route("/files/roots/{id}", delete(proxy_passthrough))
        .route("/files/roots/{id}/reactivate", post(proxy_passthrough))
        .route("/admin/keys", post(proxy_passthrough).delete(proxy_passthrough))
        .route("/admin/nodes", post(proxy_passthrough).delete(proxy_passthrough))
        .route("/admin/runtime", get(proxy_passthrough));

    Router::new()
        .merge(read_routes)
        .merge(ingest_routes)
        .merge(admin_routes)
        // Default per-IP limit for the read surface (answer path tightened
        // inside the handler via the per-key limiter).
        .layer(ratelimit::per_ip_layer(RatePolicy::READ))
        // Request-size cap (spec §12.3): reject oversized bodies early.
        .layer(RequestBodyLimitLayer::new(state.config.http.max_body_bytes))
        // CORS allow-list from config (spec §12.3); empty list = deny cross-origin.
        .layer(cors_layer(&state.config.http.cors_allowed_origins))
        .with_state(state)
}

/// Build a CORS layer from the configured allow-list (spec §12.3).
///
/// An empty allow-list yields a restrictive same-origin policy (deny-by-default,
/// spec §12.5). Origins are validated; an invalid origin is skipped (it will
/// simply not be allowed) rather than panicking.
#[must_use]
fn cors_layer(allowed_origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|o| o.parse::<HeaderValue>().ok())
        .collect();
    CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers([
            http::header::AUTHORIZATION,
            http::header::CONTENT_TYPE,
            HEADER_NAME.parse().expect("static header name is valid"),
        ])
        .allow_origin(origins)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /query/search` (scope: reader) — validate, authorize, proxy.
async fn proxy_search(
    State(state): State<GatewayState>,
    Authenticated(ctx): Authenticated,
    correlation_id: CorrelationId,
    body: axum::body::Bytes,
) -> Result<Response, GatewayError> {
    Requirement::reader().enforce(&ctx)?;
    // TODO: garde-validate the deserialized SearchRequest before proxying
    //       (spec §11.1) — reject malformed/oversized queries here.
    Ok(proxy(&state, Method::POST, "/api/v1/query/search", correlation_id, body).await?)
}

/// `POST /query/answer` (scope: reader) — the expensive grounded-answer path.
///
/// Carries the strictest per-key limit (spec §12.3): even a valid reader cannot
/// flood the LLM. We fail-closed (429) when the per-key bucket is empty.
async fn proxy_answer(
    State(state): State<GatewayState>,
    Authenticated(ctx): Authenticated,
    correlation_id: CorrelationId,
    body: axum::body::Bytes,
) -> Result<Response, GatewayError> {
    Requirement::reader().enforce(&ctx)?;
    let principal = principal_id(&ctx);
    if state.answer_limiter.check(&principal).is_err() {
        return Err(AppError::Dependency {
            dependency: "rate-limit".to_owned(),
            message: "answer rate limit exceeded".to_owned(),
        }
        .into());
    }
    Ok(proxy(&state, Method::POST, "/api/v1/query/answer", correlation_id, body).await?)
}

/// Generic authenticated pass-through proxy used by the remaining routes.
///
/// The required scope/role is derived from the path prefix so the §11.2 mapping
/// stays in one place. The original method + path are preserved upstream.
async fn proxy_passthrough(
    State(state): State<GatewayState>,
    Authenticated(ctx): Authenticated,
    correlation_id: CorrelationId,
    method: Method,
    // OriginalUri preserves the full `/api/v1/...` path (the bare `Uri` extractor
    // would drop the `/api/v1` nest prefix), so we proxy the exact path upstream.
    OriginalUri(uri): OriginalUri,
    body: axum::body::Bytes,
) -> Result<Response, GatewayError> {
    let path = uri.path().to_owned();
    requirement_for(&path).enforce(&ctx)?;
    // TODO: for root-scoped paths (/files/roots/{id}…), extract the root id and
    //       call crate::auth::check_domain_scope(&ctx.domain_scope, &root_id)
    //       before proxying (spec §12.7 domain-scope check).
    Ok(proxy(&state, method, &path, correlation_id, body).await?)
}

/// Map a `/api/v1` path to its required [`Requirement`] (spec §11.2).
fn requirement_for(path: &str) -> Requirement {
    if path.starts_with("/api/v1/admin")
        || (path.starts_with("/api/v1/files/roots/")) // DELETE / reactivate are admin
    {
        Requirement::admin()
    } else if path.starts_with("/api/v1/files/roots")
        || path.starts_with("/api/v1/ingestion")
        || path.starts_with("/api/v1/batch/submit")
    {
        Requirement::ingest()
    } else {
        Requirement::reader()
    }
}

/// Stable principal identifier for per-key rate limiting / audit.
fn principal_id(ctx: &diyrag_common::auth::AuthContext) -> String {
    if let Some(key_id) = ctx.api_key_id {
        format!("key:{key_id}")
    } else if let Some(user_id) = ctx.user_id {
        format!("user:{user_id}")
    } else {
        format!("tenant:{}", ctx.tenant_id)
    }
}

/// Forward a request to `core-api`, injecting the correlation id (spec §13.1)
/// and returning the upstream response verbatim. Network failures map to the
/// standard error envelope as a transient dependency error (spec §11.3/§14).
async fn proxy(
    state: &GatewayState,
    method: Method,
    path: &str,
    correlation_id: CorrelationId,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let url = format!("{}{}", state.core_api_base.trim_end_matches('/'), path);
    let upstream = state
        .http
        .request(method, &url)
        // Correlation-id injection: every outbound hop carries X-Correlation-ID
        // so logs/traces stitch together (spec §13.1).
        .header(HEADER_NAME, correlation_id.to_string())
        .body(body)
        .send()
        .await
        .map_err(|e| AppError::Dependency {
            dependency: "core-api".to_owned(),
            message: e.to_string(),
        })?;

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let bytes = upstream.bytes().await.map_err(|e| AppError::Dependency {
        dependency: "core-api".to_owned(),
        message: e.to_string(),
    })?;
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    response.headers_mut().insert(
        HEADER_NAME,
        HeaderValue::from_str(&correlation_id.to_string())
            .unwrap_or(HeaderValue::from_static("")),
    );
    Ok(response)
}
