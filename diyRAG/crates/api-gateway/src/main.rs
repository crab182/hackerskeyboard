#![forbid(unsafe_code)]
//! `diyrag-api-gateway` — the edge service (spec §2, §11, §12.3).
//!
//! All client traffic enters here. The gateway terminates TLS (rustls), mints/
//! propagates the `correlation_id`, authenticates and authorizes the principal,
//! enforces rate limits and request-size/schema validation, applies CORS, and
//! proxies surviving requests to `core-api`. Health endpoints are public.
//!
//! Errors use `anyhow` at the binary boundary (spec §19); the library types
//! from `diyrag-common` carry the structured envelope.

mod auth;
mod ratelimit;
mod routes;

use std::sync::Arc;

use anyhow::Context;
use axum::routing::get;
use axum::Router;
use diyrag_common::config::AppConfig;
use diyrag_common::{logging, prelude::*};
use ratelimit::{PerKeyLimiter, RatePolicy};
use tracing::info;

/// Shared, cheaply-cloneable gateway state injected into handlers.
#[derive(Clone)]
pub struct GatewayState {
    /// Loaded configuration.
    pub config: Arc<AppConfig>,
    /// HTTP client used to proxy to `core-api`.
    pub http: reqwest::Client,
    /// Base URL of the upstream `core-api`.
    pub core_api_base: String,
    /// Per-principal token-bucket limiter for the expensive answer path (§12.3).
    pub answer_limiter: Arc<PerKeyLimiter>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load typed config.
    let config = AppConfig::load(Some("config/api-gateway.toml"))
        .context("loading api-gateway configuration")?;

    // 2. Initialize structured JSON logging from diyrag-common.
    logging::init(&config.observability).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    info!(service = %config.service_name, "starting api-gateway");

    let addr = config
        .socket_addr()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    // 3. Build shared state (proxy client to core-api).
    // DECISION: core-api base URL is read from an env-driven config key; no
    // hardcoded host (spec §0). Placeholder default keeps the scaffold honest.
    let state = GatewayState {
        config: Arc::new(config),
        http: reqwest::Client::builder()
            .build()
            .context("building proxy http client")?,
        // TODO: source from config (e.g. AppConfig::upstreams.core_api).
        core_api_base: "https://core-api:8081".to_owned(),
        answer_limiter: Arc::new(PerKeyLimiter::new(RatePolicy::ANSWER)),
    };

    // 4. Build the Axum app with the full middleware stack + routes.
    let app = build_app(state);

    // 5. Serve.
    // TLS NOTE: in production the gateway terminates TLS 1.3 via rustls
    // (axum-server + RustlsConfig, mTLS east-west, spec §12.1). For the scaffold
    // we bind plain TCP; swap `axum::serve` for `axum_server::bind_rustls` once
    // certs (rcgen-issued, ≤90-day) are wired in.
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!(%addr, "api-gateway listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("api-gateway server error")?;

    Ok(())
}

/// Assemble the router: health endpoints + versioned API + middleware stack.
fn build_app(state: GatewayState) -> Router {
    let api = routes::router(state.clone());

    Router::new()
        // Public liveness/readiness (spec §11.2). No auth, no rate limit.
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // Versioned, protected API surface. CORS, request-body-size cap, per-IP
        // rate limiting, authN/Z (per-route), and schema validation are applied
        // INSIDE `routes::router` so they wrap only the protected surface and the
        // public probes stay unthrottled (spec §11.2, §12.3).
        .nest("/api/v1", api)
        // Trace layer opens a per-request span carrying the correlation id so all
        // logs within a request are attributable (spec §13.1). The correlation id
        // is then re-injected on the outbound hop to core-api in `routes::proxy`.
        .layer(logging::trace_layer())
        .with_state(state)
}

/// Liveness probe — process is up (spec §11.2).
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe — gateway can reach `core-api` (spec §11.2).
async fn readyz() -> &'static str {
    // TODO: probe upstream core-api `/healthz`; return 503 if unreachable.
    "ready"
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received; draining");
}
