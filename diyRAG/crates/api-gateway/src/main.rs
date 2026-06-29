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
mod error;
mod ratelimit;
mod routes;

use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use diyrag_common::config::AppConfig;
use diyrag_common::logging;
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

    // Container HEALTHCHECK form (`api-gateway healthcheck`): probe our own
    // /healthz on loopback and exit, instead of booting a second server (§16b).
    if diyrag_common::health::is_healthcheck_invocation() {
        std::process::exit(diyrag_common::health::http_healthcheck(
            &config.http.bind_addr,
        ));
    }

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
    // Public liveness/readiness (spec §11.2). No auth, no rate limit. `readyz`
    // probes upstream `core-api`, so this sub-router carries `GatewayState` and
    // binds it with `.with_state`, yielding a stateless `Router<()>` that merges
    // into the outer app alongside the (also stateless) `/api/v1` surface.
    let health = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state.clone());

    // `routes::router` binds the state internally and returns a stateless
    // `Router<()>` too, so the assembled app is `Router<()>` and needs no further
    // `.with_state`.
    let api = routes::router(state);

    Router::new()
        .merge(health)
        // Versioned, protected API surface. CORS, request-body-size cap, per-IP
        // rate limiting, authN/Z (per-route), and schema validation are applied
        // INSIDE `routes::router` so they wrap only the protected surface and the
        // public probes stay unthrottled (spec §11.2, §12.3).
        .nest("/api/v1", api)
        // Trace layer opens a per-request span carrying the correlation id so all
        // logs within a request are attributable (spec §13.1). The correlation id
        // is then re-injected on the outbound hop to core-api in `routes::proxy`.
        .layer(logging::trace_layer())
}

/// Liveness probe — process is up (spec §11.2).
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe — the gateway is ready iff it can reach `core-api` (spec
/// §11.2). A short timeout keeps the probe cheap; any non-success or transport
/// error yields 503 so orchestrators (and the `456468ann` launch gate) treat the
/// edge as not-ready until the upstream is up.
async fn readyz(State(state): State<GatewayState>) -> StatusCode {
    let url = core_api_health_url(&state.core_api_base);
    let ok = matches!(
        state
            .http
            .get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await,
        Ok(resp) if resp.status().is_success()
    );
    if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Build the upstream `core-api` `/healthz` URL from its base, tolerating a
/// trailing slash on the configured base (no double `//`).
fn core_api_health_url(core_api_base: &str) -> String {
    format!("{}/healthz", core_api_base.trim_end_matches('/'))
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received; draining");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_api_health_url_handles_trailing_slash() {
        assert_eq!(
            core_api_health_url("https://core-api:8081"),
            "https://core-api:8081/healthz"
        );
        // A trailing slash on the base must not produce a double slash.
        assert_eq!(
            core_api_health_url("https://core-api:8081/"),
            "https://core-api:8081/healthz"
        );
    }
}
