#![forbid(unsafe_code)]
//! `diyrag-core-api` — file/root management, batch orchestration, retention, and
//! RAG orchestration (spec §2, §6, §7, §11).
//!
//! `core-api` sits behind `api-gateway` (which owns authN/Z, rate limiting, and
//! TLS termination at the edge). It owns the authoritative metadata in Postgres,
//! publishes ingestion work units to NATS JetStream, drives retention/logical
//! delete (spec §6.6), and orchestrates query/answer by fanning out to the
//! `retrieval` and `gpu-runtime` services (spec §7).
//!
//! Errors use `anyhow` at the binary boundary (spec §19); library types from
//! `diyrag-common` carry the structured envelope.

mod batch;
mod files;
mod rag;
mod roots;

use std::sync::Arc;

use anyhow::Context;
use axum::routing::{get, post};
use axum::Router;
use diyrag_common::config::AppConfig;
use diyrag_common::{db, logging};
use sqlx::PgPool;
use tracing::info;

/// Shared, cheaply-cloneable core-api state injected into handlers and tasks.
#[derive(Clone)]
pub struct CoreState {
    /// Loaded configuration.
    pub config: Arc<AppConfig>,
    /// Postgres pool (authoritative metadata, spec §5.1).
    pub db: PgPool,
    /// NATS JetStream context for publishing work units (spec §6.2).
    pub jetstream: Arc<batch::JetStreamPublisher>,
    /// HTTP client for retrieval / gpu-runtime calls (spec §7).
    pub http: reqwest::Client,
    /// Base URL of the `retrieval` service.
    pub retrieval_base: String,
    /// Base URL of the `gpu-runtime` service.
    pub gpu_runtime_base: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load typed config.
    let config =
        AppConfig::load(Some("config/core-api.toml")).context("loading core-api configuration")?;

    // Container HEALTHCHECK form (`core-api healthcheck`): probe our own /healthz
    // on loopback and exit, instead of booting a second server (§16b).
    if diyrag_common::health::is_healthcheck_invocation() {
        std::process::exit(diyrag_common::health::http_healthcheck(
            &config.http.bind_addr,
        ));
    }

    // 2. Initialize structured JSON logging.
    logging::init(&config.observability).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    info!(service = %config.service_name, "starting core-api");

    let addr = config
        .socket_addr()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    // 3. Connect dependencies: Postgres pool + NATS JetStream publisher.
    let db = db::init_pool(&config.database)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))
        .context("connecting to postgres")?;

    if config.database.run_migrations_on_start {
        db::run_migrations(&db)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .context("running migrations")?;
    }

    let jetstream = batch::JetStreamPublisher::connect(&config.nats)
        .await
        .context("connecting to NATS JetStream")?;

    // 4. Build shared state.
    let state = CoreState {
        config: Arc::new(config),
        db,
        jetstream: Arc::new(jetstream),
        http: reqwest::Client::builder()
            .build()
            .context("building outbound http client")?,
        // TODO: source these from config (no hardcoded hosts, spec §0).
        retrieval_base: "https://retrieval:8082".to_owned(),
        gpu_runtime_base: "https://gpu-runtime:8090".to_owned(),
    };

    // 5. Spawn the folder-root file watcher (spec §6.1). Runs for the process
    //    lifetime; cooperatively cancelled on shutdown.
    let watcher_state = state.clone();
    let watcher_handle = tokio::spawn(async move {
        if let Err(e) = files::run_watcher(watcher_state).await {
            tracing::error!(error = %e, "file watcher exited with error");
        }
    });

    // 6. Build the Axum app and serve.
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!(%addr, "core-api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("core-api server error")?;

    watcher_handle.abort();
    Ok(())
}

/// Assemble the router: health endpoints + the §11.2 surface (already authorized
/// upstream by `api-gateway`).
fn build_app(state: CoreState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // File / root management (spec §6.1, §6.6, §11.2).
        .route("/api/v1/files/roots", post(files::register_root))
        .route(
            "/api/v1/files/roots/{id}",
            axum::routing::delete(roots::deactivate_root),
        )
        .route(
            "/api/v1/files/roots/{id}/reactivate",
            post(roots::reactivate_root),
        )
        .route("/api/v1/ingestion/trigger", post(files::trigger_ingest))
        // Batch orchestration (spec §6.7, §11.2).
        .route("/api/v1/batch/submit", post(batch::submit_batch))
        .route("/api/v1/batch/{job_id}/status", get(batch::batch_status))
        // RAG orchestration (spec §7, §11.2).
        .route("/api/v1/query/search", post(rag::search))
        .route("/api/v1/query/answer", post(rag::answer))
        .layer(logging::trace_layer())
        .with_state(state)
}

/// Liveness probe (spec §11.2).
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe: Postgres reachable (spec §11.2).
async fn readyz(
    axum::extract::State(state): axum::extract::State<CoreState>,
) -> axum::http::StatusCode {
    match db::ping(&state.db).await {
        Ok(()) => axum::http::StatusCode::OK,
        Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received; draining");
}
