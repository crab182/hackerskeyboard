#![forbid(unsafe_code)]
//! Vector + blob replication (MASTER_BUILD_SPEC.md §9, §22 #11).
//!
//! Two replication channels keep peers' **retrievable corpora** identical:
//!   1. **Qdrant snapshots** (per-collection): a peer lists the snapshots it can
//!      offer (`SnapshotManifest`), the local node pulls the ones it lacks and
//!      applies (restores) them. Vectors are deterministic given
//!      `(blob + model + chunker config)`, so a peer running a *different*
//!      embedding model may instead **re-embed from synced blobs** — we record
//!      `embed_model` per snapshot to detect the mismatch (§9 / §22 #11).
//!   2. **Content-addressed blobs**: missing originals are fetched **by sha256**
//!      on demand, streamed in bounded chunks over mTLS (§5.3 / §9).
//!
//! All applied content lands as `retention_status = ACTIVE` only after a healed
//! partition converges (acceptance #5); the query layer's ACTIVE filter (§6.6)
//! means in-flight replication never leaks partial corpora into results.

use diyrag_common::errors::AppError;

/// A per-collection snapshot offered by (or needed from) a peer (§9).
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    /// Per-tenant Qdrant collection name `t_{slug}` (spec §5.2).
    pub collection: String,
    /// Opaque Qdrant snapshot id.
    pub snapshot_id: String,
    /// SHA-256 of the snapshot bytes; verified before apply (integrity).
    pub snapshot_sha256: String,
    /// Size in bytes (bound checks before pulling).
    pub size_bytes: u64,
    /// Embedding model the vectors were produced with — drift detector (§22 #11).
    pub embed_model: String,
}

/// What to do with a peer's snapshot given our local embedding model (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPlan {
    /// Same model → restore the Qdrant snapshot directly (fast path).
    RestoreSnapshot,
    /// Different model → re-embed from synced blobs to keep vectors comparable.
    ReembedFromBlobs,
}

/// Decide how to apply a peer snapshot: restore if the embedding models match,
/// otherwise re-embed from blobs (§9 / §22 #11). Pure + unit-testable.
#[must_use]
pub fn plan_apply(local_embed_model: &str, snapshot: &SnapshotEntry) -> ApplyPlan {
    if local_embed_model == snapshot.embed_model {
        ApplyPlan::RestoreSnapshot
    } else {
        ApplyPlan::ReembedFromBlobs
    }
}

/// Replicates Qdrant snapshots and blobs from approved peers.
///
/// Holds the local Qdrant client + blob store (from `diyrag-common`) and the
/// gRPC client factory. Construction is deferred until wiring is complete.
pub struct Replicator {
    /// Embedding model this node runs (for the drift check, §22 #11).
    local_embed_model: String,
}

impl Replicator {
    /// Build a replicator bound to the local embedding model.
    #[must_use]
    pub fn new(local_embed_model: String) -> Self {
        Self { local_embed_model }
    }

    /// Pull and apply any snapshots the local node is missing for `collection`
    /// from the given peer (§9 vector payload).
    pub async fn pull_and_apply_snapshot(
        &self,
        peer_endpoint: &str,
        snapshot: &SnapshotEntry,
    ) -> Result<ApplyPlan, AppError> {
        let plan = plan_apply(&self.local_embed_model, snapshot);
        let _ = peer_endpoint;
        match plan {
            ApplyPlan::RestoreSnapshot => {
                // TODO: stream `FetchSnapshot` chunks over mTLS into a temp file,
                // verify snapshot_sha256, then `qdrant_client` recover-from-snapshot
                // (create_snapshot/recover_snapshot APIs) atomically (build new
                // collection, alias swap per §7.3). Map errors to
                // AppError::Dependency { dependency: "qdrant", .. }.
            }
            ApplyPlan::ReembedFromBlobs => {
                // TODO: enumerate the registry rows for this collection, fetch any
                // missing blobs by hash (`fetch_blob`), and enqueue a REINDEX job
                // so the local embedding model produces comparable vectors (§7.3).
            }
        }
        Ok(plan)
    }

    /// Fetch a content-addressed blob by its sha256 from a peer, streaming in
    /// bounded chunks over mTLS, and persist it to the local blob store (§5.3 / §9).
    pub async fn fetch_blob(&self, peer_endpoint: &str, content_sha256: &str) -> Result<(), AppError> {
        let _ = (peer_endpoint, content_sha256);
        // TODO: derive key `sha256/{first2}/{sha256}` (mirror common::blob), skip
        // if already present locally (content-addressed dedup), else stream
        // `FetchBlob` chunks and verify the hash of the assembled bytes before
        // committing to object_store. Reject a hash mismatch as
        // AppError::Unprocessable (poisoned/corrupt blob, §12.5).
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(model: &str) -> SnapshotEntry {
        SnapshotEntry {
            collection: "t_acme".to_owned(),
            snapshot_id: "snap-1".to_owned(),
            snapshot_sha256: "deadbeef".to_owned(),
            size_bytes: 1024,
            embed_model: model.to_owned(),
        }
    }

    #[test]
    fn same_model_restores_snapshot() {
        assert_eq!(plan_apply("bge-m3", &snap("bge-m3")), ApplyPlan::RestoreSnapshot);
    }

    #[test]
    fn different_model_reembeds() {
        assert_eq!(plan_apply("bge-m3", &snap("e5-large")), ApplyPlan::ReembedFromBlobs);
    }
}
