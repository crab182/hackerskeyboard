#![forbid(unsafe_code)]
//! RAG orchestration: search + grounded answer (spec §7).
//!
//! `core-api` is the orchestrator: it calls the `retrieval` service (hybrid
//! search → rerank → optional condense) and the `gpu-runtime` service (grounded
//! generation with citations), then assembles the structured envelope returned
//! to the client (spec §7.2). It never embeds or generates in-process — those
//! are owned by the `retrieval` and `gpu-runtime` services respectively.
//!
//! This module also defines [`ApiError`], the local Axum error wrapper used by
//! every core-api handler (the orphan rule forbids implementing `IntoResponse`
//! for the foreign [`AppError`] directly).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use diyrag_common::correlation::CorrelationId;
use diyrag_common::errors::AppError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::CoreState;

/// Local handler error newtype rendering the standard envelope (spec §11.3).
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
        // TODO: write the error_log row and use its log_id as the reference_code
        //       so the GUI deep-link resolves (spec §10.4 / §13.2).
        let envelope = err.to_envelope("ERR-PENDING", CorrelationId::new());
        (status, Json(envelope)).into_response()
    }
}

/// Caller-supplied retrieval filters echoed to the `retrieval` service. Tenant
/// scoping is applied server-side and is NOT part of this struct (spec §12.7).
#[derive(Debug, Clone, Default, Serialize, Deserialize, garde::Validate)]
pub struct QueryFilters {
    /// Optional restriction to specific roots (re-checked against domain scope).
    #[garde(skip)]
    pub root_ids: Vec<Uuid>,
    /// Optional language filter.
    #[garde(length(max = 16))]
    pub lang: Option<String>,
}

/// `POST /api/v1/query/search` body (spec §11.2, scope: reader).
#[derive(Debug, Clone, Deserialize, garde::Validate)]
pub struct SearchRequest {
    /// Natural-language query.
    #[garde(length(min = 1, max = 8192))]
    pub query: String,
    /// Number of reranked results to return (k, spec §7.1: 8–12 typical).
    #[garde(range(min = 1, max = 100))]
    pub k: u32,
    /// Optional filters.
    #[garde(dive)]
    #[serde(default)]
    pub filters: QueryFilters,
}

/// A single reranked, cited result (spec §7.1 / §7.2).
#[derive(Debug, Clone, Serialize)]
pub struct RetrievedChunk {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub text: String,
    pub score: f32,
    pub page_number: Option<i32>,
}

/// `query/search` response envelope.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub success: bool,
    pub results: Vec<RetrievedChunk>,
}

/// `POST /api/v1/query/answer` body (spec §11.2, scope: reader).
#[derive(Debug, Clone, Deserialize, garde::Validate)]
pub struct AnswerRequest {
    #[garde(length(min = 1, max = 8192))]
    pub query: String,
    #[garde(range(min = 1, max = 100))]
    pub k: u32,
    #[garde(dive)]
    #[serde(default)]
    pub filters: QueryFilters,
}

/// A claim → source mapping for inline citations (spec §7.2).
#[derive(Debug, Clone, Serialize)]
pub struct Citation {
    pub document_id: Uuid,
    pub page_number: Option<i32>,
}

/// `query/answer` response envelope with mandatory citations (spec §7.2).
#[derive(Debug, Clone, Serialize)]
pub struct AnswerResponse {
    pub success: bool,
    pub answer: String,
    pub citations: Vec<Citation>,
    /// Conflicting sources are flagged, never averaged (spec §7.2).
    pub conflicts: Vec<String>,
}

/// `POST /api/v1/query/search` — hybrid retrieval via the `retrieval` service.
pub async fn search(
    State(state): State<CoreState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    garde::Validate::validate(&req).map_err(|e| AppError::Validation {
        message: e.to_string(),
    })?;

    // TODO:
    //   1. derive tenant from the authenticated principal (forwarded by gateway,
    //      spec §12.7) — NEVER from req,
    //   2. POST to {retrieval_base}/search with {query, k0=40, filters} so the
    //      retrieval service embeds + hybrid-searches + reranks (spec §7.1),
    //   3. map the reranked hits into RetrievedChunk.
    let _ = (&state.http, &state.retrieval_base);

    Ok(Json(SearchResponse {
        success: true,
        results: Vec::new(),
    }))
}

/// `POST /api/v1/query/answer` — grounded generation (spec §7.2).
pub async fn answer(
    State(state): State<CoreState>,
    Json(req): Json<AnswerRequest>,
) -> Result<Json<AnswerResponse>, ApiError> {
    garde::Validate::validate(&req).map_err(|e| AppError::Validation {
        message: e.to_string(),
    })?;

    // TODO:
    //   1. call retrieval (as in `search`) for reranked chunks,
    //   2. optional context-condense pass to prevent context-window collapse
    //      (spec §7.2),
    //   3. POST to {gpu_runtime_base}/infer with a prompt that STRICTLY separates
    //      trusted instructions from retrieved (untrusted) content via explicit
    //      delimiters + trust markers (spec §7.2 / §12.5),
    //   4. require inline citations mapping each claim -> document_id+page_number
    //      and flag conflicting sources rather than averaging (spec §7.2).
    let _ = (&state.http, &state.retrieval_base, &state.gpu_runtime_base);

    Ok(Json(AnswerResponse {
        success: true,
        answer: String::new(),
        citations: Vec::new(),
        conflicts: Vec::new(),
    }))
}
