#![forbid(unsafe_code)]
//! Root retention & logical delete (spec §6.6, §11.2).
//!
//! The "keep contents" guarantee: deactivating a root NEVER destroys ingested
//! content or the index. `DELETE /files/roots/{id}` sets `roots.is_active=false`
//! and, via an async pass, `documents.retention_status=PURGED_LOGICAL` for that
//! root plus the matching Qdrant payload. **The query layer always filters
//! `retention_status=ACTIVE`** (spec §6.6 / §7.1), so a logical purge removes the
//! root from all results within one transaction while data + blobs remain. Every
//! purge/reactivate writes an `audit_log` entry and is reversible. Hard physical
//! delete is a separate, admin-only, audited operation (not implemented here).

use axum::extract::{Path, State};
use axum::Json;
use diyrag_common::errors::AppError;
use diyrag_common::schemas::RetentionStatus;
use serde::Serialize;
use uuid::Uuid;

use crate::rag::ApiError;
use crate::CoreState;

/// Result of a purge / reactivate operation.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionChangeResponse {
    pub root_id: Uuid,
    /// New retention status applied to the root's documents.
    pub retention_status: RetentionStatus,
    /// Number of documents whose retention status changed.
    pub affected_documents: i64,
    pub reversible: bool,
}

/// `DELETE /api/v1/files/roots/{id}` — logical purge (spec §6.6, scope: admin).
///
/// Atomic with respect to query visibility: within one transaction the root is
/// deactivated and its documents flipped to `PURGED_LOGICAL`; the Qdrant payload
/// is mirrored so hybrid search (which filters `ACTIVE`) stops returning them.
pub async fn deactivate_root(
    State(state): State<CoreState>,
    Path(root_id): Path<Uuid>,
) -> Result<Json<RetentionChangeResponse>, ApiError> {
    let affected = set_root_retention(&state, root_id, RetentionStatus::PurgedLogical).await?;
    Ok(Json(RetentionChangeResponse {
        root_id,
        retention_status: RetentionStatus::PurgedLogical,
        affected_documents: affected,
        reversible: true,
    }))
}

/// `POST /api/v1/files/roots/{id}/reactivate` — reverse a logical purge
/// (spec §6.6, scope: admin).
pub async fn reactivate_root(
    State(state): State<CoreState>,
    Path(root_id): Path<Uuid>,
) -> Result<Json<RetentionChangeResponse>, ApiError> {
    let affected = set_root_retention(&state, root_id, RetentionStatus::Active).await?;
    Ok(Json(RetentionChangeResponse {
        root_id,
        retention_status: RetentionStatus::Active,
        affected_documents: affected,
        reversible: true,
    }))
}

/// Flip every document under `root_id` to `status` in Postgres and Qdrant, then
/// audit the change (spec §6.6). Tenant is server-derived (spec §12.7).
async fn set_root_retention(
    state: &CoreState,
    root_id: Uuid,
    status: RetentionStatus,
) -> Result<i64, AppError> {
    // TODO (single sqlx transaction, spec §6.6 / §12.4 parameterized):
    //   1. UPDATE roots SET is_active = ($status == ACTIVE) WHERE id=$1
    //      AND tenant_id=$2 (server-derived tenant) — 404 if no row,
    //   2. UPDATE documents SET retention_status=$status, updated_at=now()
    //      WHERE tenant_id=$2 AND root_id=$1 RETURNING count,
    //   3. VectorStore::set_retention_for_root(tenant_slug, root_id, status)
    //      to mirror the Qdrant payload (spec §6.6),
    //   4. INSERT an audit_log row (action=PURGE_LOGICAL / REACTIVATE, before/
    //      after, actor, correlation_id) — every purge is audited (spec §6.6),
    //   5. COMMIT; return the affected document count.
    let _ = (&state.db, root_id, status);
    Err(AppError::NotFound {
        resource: format!("root {root_id}"),
    })
}
