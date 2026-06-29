#![forbid(unsafe_code)]
//! `diyrag-retrieval` — hybrid search + rerank + optional condense (spec §7).
//!
//! Called by `core-api`. Given a query embedding (dense + sparse, BGE-M3) the
//! service runs Qdrant hybrid search scoped to the caller's tenant collection
//! and filtered to `retention_status = ACTIVE` (spec §7.1 / §12.7), reranks the
//! top-`k₀` (≈40) hits with a `bge-reranker-v2-m3` cross-encoder down to `k`
//! (8–12), and optionally condenses the surviving context (spec §7.2). It owns
//! no answer generation — that is `gpu-runtime`.
//!
//! Errors use `anyhow` at the binary boundary (spec §19).

mod condense;
mod hybrid;
mod rerank;

use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use diyrag_common::config::AppConfig;
use diyrag_common::correlation::CorrelationId;
use diyrag_common::errors::AppError;
use diyrag_common::logging;
use diyrag_common::vector::QdrantStore;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

/// Shared, cheaply-cloneable retrieval state.
#[derive(Clone)]
pub struct RetrievalState {
    /// Loaded configuration.
    pub config: Arc<AppConfig>,
    /// Qdrant-backed vector store (per-tenant collections, spec §5.2).
    pub store: Arc<QdrantStore>,
    /// The reranker backend (in-proc candle or gpu-runtime HTTP, spec §7.1).
    pub reranker: Arc<rerank::Reranker>,
    /// HTTP client for the embedding/condense backend (gpu-runtime, spec §16).
    pub http: reqwest::Client,
    /// Base URL of the embedding/condense backend.
    pub gpu_runtime_base: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::load(Some("config/retrieval.toml"))
        .context("loading retrieval configuration")?;

    // Container HEALTHCHECK form (`retrieval healthcheck`): probe our own /healthz
    // on loopback and exit, instead of booting a second server (§16b).
    if diyrag_common::health::is_healthcheck_invocation() {
        std::process::exit(diyrag_common::health::http_healthcheck(
            &config.http.bind_addr,
        ));
    }

    logging::init(&config.observability).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    info!(service = %config.service_name, "starting retrieval");

    let addr = config
        .socket_addr()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let store = QdrantStore::connect(&config.qdrant)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))
        .context("connecting to qdrant")?;

    let reranker = rerank::Reranker::init(&config)
        .await
        .context("initializing reranker backend")?;

    let state = RetrievalState {
        config: Arc::new(config),
        store: Arc::new(store),
        reranker: Arc::new(reranker),
        http: reqwest::Client::builder()
            .build()
            .context("building outbound http client")?,
        // TODO: source from config (no hardcoded hosts, spec §0).
        gpu_runtime_base: "https://gpu-runtime:8090".to_owned(),
    };

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!(%addr, "retrieval listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("retrieval server error")?;

    Ok(())
}

/// Assemble the router: health endpoints + the internal retrieval surface.
fn build_app(state: RetrievalState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // Internal endpoint called by core-api (spec §7.1). Not client-facing;
        // the gateway never proxies here directly.
        .route("/search", post(search))
        .layer(logging::trace_layer())
        .with_state(state)
}

/// `POST /search` request: tenant is provided by the trusted caller (core-api),
/// which derived it server-side from the authenticated principal (spec §12.7).
#[derive(Debug, Clone, Deserialize, garde::Validate)]
pub struct SearchRequest {
    /// Tenant slug → per-tenant Qdrant collection (spec §5.2). Trusted: set by
    /// core-api from the verified principal, never by an end client.
    #[garde(length(min = 1, max = 128))]
    pub tenant_slug: String,
    /// Natural-language query to embed + search.
    #[garde(length(min = 1, max = 8192))]
    pub query: String,
    /// Final number of reranked results to return (`k`, spec §7.1).
    #[garde(range(min = 1, max = 100))]
    pub k: usize,
    /// Optional restriction to specific roots.
    #[garde(skip)]
    #[serde(default)]
    pub root_ids: Vec<Uuid>,
    /// Optional language filter.
    #[garde(length(max = 16))]
    pub lang: Option<String>,
    /// Whether to run the optional context-condense pass (spec §7.2).
    #[garde(skip)]
    #[serde(default)]
    pub condense: bool,
}

/// A reranked, returnable hit (spec §7.1).
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub score: f32,
    pub page_number: Option<i32>,
    pub text: String,
}

/// `POST /search` response.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// Condensed context, present only when `condense = true` was requested.
    pub condensed_context: Option<String>,
}

/// The retrieval pipeline: embed → hybrid search (k₀) → rerank (k) → condense.
async fn search(
    State(state): State<RetrievalState>,
    correlation_id: CorrelationId,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    garde::Validate::validate(&req).map_err(|e| AppError::Validation {
        message: e.to_string(),
    })?;

    // 1. Hybrid dense+sparse search scoped to the tenant collection, filtered to
    //    retention_status = ACTIVE (spec §7.1 / §12.7).
    let scored = hybrid::hybrid_search(&state, &req, correlation_id).await?;

    // 2. Rerank the top-k₀ down to k (spec §7.1).
    let reranked = state.reranker.rerank(&req.query, scored, req.k).await?;

    // 3. Optional context-condense pass (spec §7.2).
    let condensed_context = if req.condense {
        Some(condense::condense(&state, &req.query, &reranked).await?)
    } else {
        None
    };

    Ok(Json(SearchResponse {
        hits: reranked,
        condensed_context,
    }))
}

/// Liveness probe (spec §11.2).
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe: the vector backend is reachable (spec §11.2).
async fn readyz(State(state): State<RetrievalState>) -> StatusCode {
    use diyrag_common::vector::VectorStore;
    match state.store.health().await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received; draining");
}

/// Local handler error newtype rendering the standard envelope (spec §11.3).
/// The orphan rule forbids `impl IntoResponse for AppError` directly.
pub struct ApiError(pub AppError);

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let err = self.0;
        let status =
            StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let envelope = err.to_envelope("ERR-PENDING", CorrelationId::new());
        (status, Json(envelope)).into_response()
    }
}
