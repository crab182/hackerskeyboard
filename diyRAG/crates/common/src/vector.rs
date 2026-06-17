#![forbid(unsafe_code)]
//! Vector store abstraction (spec §5.2, §7.1).
//!
//! A swappable [`VectorStore`] trait (spec §19 lists `VectorStore` among the
//! trait-gated, replaceable modules) plus a Qdrant implementation skeleton.
//!
//! Hard tenant isolation is **one collection per tenant** named
//! `{prefix}{tenant_slug}` (spec §5.2/§12.7) — never payload-only filtering.
//! Named vectors `dense` and `sparse` (BGE-M3) drive native hybrid search with
//! server-side RRF fusion; queries always filter `retention_status = ACTIVE`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::QdrantConfig;
use crate::errors::AppError;
use crate::schemas::{RetentionStatus, StructureType};

/// A dense embedding vector (BGE-M3 dense, cosine; spec §5.2).
pub type DenseVector = Vec<f32>;

/// A learned sparse vector as (index, weight) pairs (BGE-M3 sparse; spec §5.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SparseVector {
    pub indices: Vec<u32>,
    pub values: Vec<f32>,
}

/// Filterable payload stored alongside each point (spec §5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointPayload {
    pub document_id: Uuid,
    pub root_id: Option<Uuid>,
    pub retention_status: RetentionStatus,
    pub lang: Option<String>,
    pub structure_type: StructureType,
    pub page_number: Option<i32>,
    pub ingestion_ts: chrono::DateTime<chrono::Utc>,
    pub source_sha256: String,
}

/// A point to upsert: id mirrors `chunks.vector_id` (spec §5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPoint {
    pub id: Uuid,
    pub dense: DenseVector,
    pub sparse: SparseVector,
    pub payload: PointPayload,
}

/// Caller-supplied retrieval filters (spec §7.1). Tenant scoping is applied
/// server-side and is **not** part of this struct (spec §12.7).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryFilter {
    /// Optional restriction to specific roots.
    pub root_ids: Vec<Uuid>,
    /// Optional language filter.
    pub lang: Option<String>,
}

/// A hybrid query: both modalities are sent for server-side fusion (spec §7.1).
#[derive(Debug, Clone)]
pub struct HybridQuery {
    pub dense: DenseVector,
    pub sparse: SparseVector,
    /// Initial fan-out before rerank, e.g. `k0 = 40` (spec §7.1).
    pub limit: u64,
    pub filter: QueryFilter,
}

/// A single scored search hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredPoint {
    pub id: Uuid,
    pub score: f32,
    pub payload: PointPayload,
}

/// Swappable vector-store interface (spec §19).
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Ensure the per-tenant collection exists with named `dense`/`sparse`
    /// vectors, quantization, and payload indexes (spec §5.2).
    async fn ensure_collection(&self, tenant_slug: &str) -> Result<(), AppError>;

    /// Upsert points into the tenant collection; idempotent on `VectorPoint::id`.
    async fn upsert(&self, tenant_slug: &str, points: &[VectorPoint]) -> Result<(), AppError>;

    /// Hybrid dense+sparse search with server-side RRF fusion, always scoped to
    /// the tenant collection and filtered to `retention_status = ACTIVE`.
    async fn hybrid_search(
        &self,
        tenant_slug: &str,
        query: &HybridQuery,
    ) -> Result<Vec<ScoredPoint>, AppError>;

    /// Flip the `retention_status` payload on every point of a root to the given
    /// value (spec §6.6 logical purge / reactivate).
    async fn set_retention_for_root(
        &self,
        tenant_slug: &str,
        root_id: &Uuid,
        status: RetentionStatus,
    ) -> Result<(), AppError>;

    /// Liveness/readiness probe for the vector backend.
    async fn health(&self) -> Result<(), AppError>;
}

/// Qdrant-backed [`VectorStore`] (spec §3.2 `qdrant-client`).
pub struct QdrantStore {
    // TODO: hold a `qdrant_client::Qdrant` client and the configured collection
    // prefix. Construction is deferred to `connect`.
    collection_prefix: String,
}

impl QdrantStore {
    /// Connect to Qdrant using [`QdrantConfig`].
    pub async fn connect(cfg: &QdrantConfig) -> Result<Self, AppError> {
        // TODO: build the client via qdrant_client::Qdrant::from_url(&cfg.url)
        // (+ optional api_key) and perform a health check. Map errors to
        // AppError::Dependency { dependency: "qdrant", .. }.
        Ok(Self {
            collection_prefix: cfg.collection_prefix.clone(),
        })
    }

    /// Compute the per-tenant collection name `{prefix}{tenant_slug}` (spec §5.2).
    #[must_use]
    pub fn collection_name(&self, tenant_slug: &str) -> String {
        format!("{}{tenant_slug}", self.collection_prefix)
    }
}

#[async_trait]
impl VectorStore for QdrantStore {
    async fn ensure_collection(&self, _tenant_slug: &str) -> Result<(), AppError> {
        // TODO: create_collection if absent with named vectors dense (cosine) +
        // sparse, scalar/binary quantization, on_disk originals, and payload
        // indexes on root_id/retention_status/document_id (spec §5.2).
        Err(AppError::Internal {
            message: "QdrantStore::ensure_collection not yet implemented".to_owned(),
        })
    }

    async fn upsert(&self, _tenant_slug: &str, _points: &[VectorPoint]) -> Result<(), AppError> {
        // TODO: map VectorPoint -> qdrant PointStruct (named vectors + payload)
        // and call upsert_points; idempotent on point id.
        Err(AppError::Internal {
            message: "QdrantStore::upsert not yet implemented".to_owned(),
        })
    }

    async fn hybrid_search(
        &self,
        _tenant_slug: &str,
        _query: &HybridQuery,
    ) -> Result<Vec<ScoredPoint>, AppError> {
        // TODO: build a qdrant `query` (prefetch dense + sparse, FusionType::Rrf),
        // always-on filter retention_status == ACTIVE plus caller filters
        // (spec §7.1). Tenant collection only (spec §12.7).
        Err(AppError::Internal {
            message: "QdrantStore::hybrid_search not yet implemented".to_owned(),
        })
    }

    async fn set_retention_for_root(
        &self,
        _tenant_slug: &str,
        _root_id: &Uuid,
        _status: RetentionStatus,
    ) -> Result<(), AppError> {
        // TODO: set_payload with a filter on root_id (spec §6.6).
        Err(AppError::Internal {
            message: "QdrantStore::set_retention_for_root not yet implemented".to_owned(),
        })
    }

    async fn health(&self) -> Result<(), AppError> {
        // TODO: call the qdrant health/healthz endpoint.
        Err(AppError::Internal {
            message: "QdrantStore::health not yet implemented".to_owned(),
        })
    }
}
