//! Atomic persistence of chunks + vectors (MASTER_BUILD_SPEC.md §6.5).
//!
//! A chunk row (Postgres via a `sqlx` transaction) and its Qdrant point are
//! written as one logical unit, retried together, and **idempotent on
//! `vector_id`** so reprocessing never duplicates data (§6.2, §21).

use crate::chunker::Chunk;
use crate::embed::Embedding;
use crate::WorkUnit;

/// Persistence failures. `Transient` (DB/Qdrant unavailable) retries; `Permanent`
/// (constraint violation that isn't the idempotency key) quarantines (§14).
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("transient persistence failure: {0}")]
    Transient(String),
    #[error("permanent persistence failure: {0}")]
    Permanent(String),
}

/// Idempotency gate: is there already an `INDEXED` document for
/// `(tenant_id, content_sha256)`? (§6.2). If so the worker acks without work.
pub async fn is_already_indexed(
    /* db: &sqlx::PgPool, */ unit: &WorkUnit,
) -> Result<bool, PersistError> {
    // TODO: SELECT 1 FROM documents
    //       WHERE tenant_id = $1 AND content_sha256 = $2 AND status = 'INDEXED'
    //       (parameterized — §12.4). Returns true if found.
    let _ = unit;
    Ok(false)
}

/// Persist all chunks for a document atomically (§6.5).
///
/// Per chunk: upsert the `chunks` row inside a `sqlx` tx, then upsert the Qdrant
/// point with id = `vector_id`. Qdrant upserts are idempotent by id; the SQL
/// upsert is `ON CONFLICT (vector_id) DO NOTHING`. The document row flips to
/// `INDEXED` only after both stores succeed for every chunk.
pub async fn persist_chunks(
    // db: &sqlx::PgPool,
    // qdrant: &qdrant_client::Qdrant,
    unit: &WorkUnit,
    chunks: &[Chunk],
    embeddings: &[Embedding],
) -> Result<(), PersistError> {
    if chunks.len() != embeddings.len() {
        return Err(PersistError::Permanent(
            "chunk/embedding count mismatch".into(),
        ));
    }
    // TODO:
    //   let mut tx = db.begin().await?;
    //   for (chunk, emb) in chunks.iter().zip(embeddings) {
    //       upsert chunk row (parameterized, ON CONFLICT (vector_id) DO NOTHING);
    //       build a Qdrant PointStruct with named vectors {dense, sparse} and the
    //       filterable payload of §5.2 (document_id, root_id, retention_status=ACTIVE,
    //       lang, structure_type, page_number, ingestion_ts, source_sha256);
    //   }
    //   qdrant.upsert_points(collection = t_{tenant_slug}, points).await?;  // idempotent on id
    //   sqlx: UPDATE documents SET status='INDEXED', indexed_at=now() WHERE id=...;
    //   tx.commit().await?;
    let _ = (unit, chunks, embeddings);
    Ok(())
}
