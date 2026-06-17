#![forbid(unsafe_code)]
//! Hybrid dense+sparse retrieval over Qdrant (spec §7.1).
//!
//! The query is embedded into BGE-M3 **dense + sparse** vectors, then a single
//! Qdrant `query` request fetches both modalities and fuses them server-side
//! (RRF or weighted). Two invariants are MANDATORY and applied server-side:
//!
//! 1. **`retention_status = ACTIVE` filter** on every query (spec §6.6 / §7.1) —
//!    logically purged content is never returned.
//! 2. **Tenant-collection scoping** — the collection is `{prefix}{tenant_slug}`
//!    derived from the trusted `tenant_slug` set by core-api from the verified
//!    principal, never from an end-client parameter (spec §5.2 / §12.7).

use diyrag_common::correlation::CorrelationId;
use diyrag_common::errors::AppError;
use diyrag_common::vector::{DenseVector, HybridQuery, QueryFilter, SparseVector, VectorStore};

use crate::{RetrievalState, SearchHit, SearchRequest};

/// Fusion strategy for combining dense and sparse rankings (spec §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fusion {
    /// Reciprocal Rank Fusion (rank-based; robust default).
    Rrf,
    /// Linear weighted score fusion (requires comparable score scales).
    Weighted,
}

impl Default for Fusion {
    fn default() -> Self {
        Fusion::Rrf
    }
}

/// Initial fan-out before reranking (`k₀`, spec §7.1).
pub const DEFAULT_K0: u64 = 40;

/// Run the hybrid search stage and return scored hits for reranking.
///
/// Embeds the query, builds a [`HybridQuery`] with the caller's optional
/// root/lang filters, and delegates to the [`VectorStore`] (which enforces the
/// tenant collection + `retention_status = ACTIVE` filter, spec §7.1/§12.7).
pub async fn hybrid_search(
    state: &RetrievalState,
    req: &SearchRequest,
    _correlation_id: CorrelationId,
) -> Result<Vec<SearchHit>, AppError> {
    // 1. Embed the query into dense + sparse vectors (BGE-M3, spec §7.1).
    let (dense, sparse) = embed_query(state, &req.query).await?;

    // 2. Build the hybrid query with caller filters. Tenant scoping and the
    //    ACTIVE filter are NOT here — they are applied inside the store
    //    (spec §12.7), so they cannot be bypassed by a crafted request.
    let query = HybridQuery {
        dense,
        sparse,
        limit: DEFAULT_K0,
        filter: QueryFilter {
            root_ids: req.root_ids.clone(),
            lang: req.lang.clone(),
        },
    };

    // 3. Execute against the tenant collection.
    let scored = state.store.hybrid_search(&req.tenant_slug, &query).await?;

    // 4. Map ScoredPoint -> SearchHit. The chunk text is hydrated from Postgres
    //    by the caller layer or here; payload carries document_id/page_number.
    let hits = scored
        .into_iter()
        .map(|p| SearchHit {
            // DECISION: the Qdrant point id IS chunks.vector_id (spec §5.2). We
            // surface it as chunk_id here; core-api joins to the chunk row.
            chunk_id: p.id,
            document_id: p.payload.document_id,
            score: p.score,
            page_number: p.payload.page_number,
            // TODO: hydrate text from chunks.text via sqlx (text is not stored in
            //       the vector payload). Empty until that join is wired.
            text: String::new(),
        })
        .collect();

    Ok(hits)
}

/// Embed a query string into BGE-M3 dense + sparse vectors (spec §7.1 / §16).
///
/// DECISION: embedding can run in-process via `ort`/`fastembed` OR be delegated
/// to `gpu-runtime` `/embed`. We default to the gpu-runtime HTTP path here so the
/// retrieval service stays light; swap to in-proc ORT behind the same signature.
async fn embed_query(
    state: &RetrievalState,
    _query: &str,
) -> Result<(DenseVector, SparseVector), AppError> {
    // TODO: POST {gpu_runtime_base}/embed { texts: [query], modality: "query" }
    //       and parse dense + sparse vectors; map transport failures to
    //       AppError::Dependency { dependency: "gpu-runtime", .. } (spec §14).
    let _ = (&state.http, &state.gpu_runtime_base);
    Err(AppError::Dependency {
        dependency: "gpu-runtime".to_owned(),
        message: "embed_query not yet implemented".to_owned(),
    })
}
