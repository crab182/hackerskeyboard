#![forbid(unsafe_code)]
//! Batch processing + the NATS JetStream work-unit publisher (spec §6.7, §6.2).
//!
//! `POST /batch/submit` accepts archives (ZIP/TAR) or path lists, decompresses in
//! a sandbox with **size / compression-ratio / depth caps** (zip-bomb guard,
//! spec §6.7 / §22 #6), fans out one idempotent work unit per file keyed by
//! `content_sha256`, records `total_units`, and publishes to NATS. `GET
//! /batch/{job_id}/status` reports `processed/total`, `failed_unit_count`, and
//! an ETA; the job lands `COMPLETE` or `PARTIAL_FAILURE` past a threshold
//! (default 20%, spec §6.7).

use axum::extract::{Path, State};
use axum::Json;
use diyrag_common::config::NatsConfig;
use diyrag_common::errors::AppError;
use diyrag_common::ids::new_id;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::rag::ApiError;
use crate::CoreState;

/// Decompression safety caps (spec §6.7 / §22 #6). Conservative defaults; all
/// configurable per deployment.
#[derive(Debug, Clone, Copy)]
pub struct BombGuard {
    /// Maximum total uncompressed bytes across the whole archive.
    pub max_total_uncompressed: u64,
    /// Maximum uncompressed bytes for any single entry.
    pub max_entry_uncompressed: u64,
    /// Maximum overall compression ratio (uncompressed / compressed).
    pub max_ratio: u64,
    /// Maximum nested-archive / directory depth.
    pub max_depth: u32,
    /// Maximum number of entries.
    pub max_entries: u64,
}

impl Default for BombGuard {
    fn default() -> Self {
        Self {
            max_total_uncompressed: 8 * 1024 * 1024 * 1024, // 8 GiB
            max_entry_uncompressed: 1024 * 1024 * 1024,     // 1 GiB
            max_ratio: 200,
            max_depth: 16,
            max_entries: 100_000,
        }
    }
}

impl BombGuard {
    /// Validate a running tally against the caps; returns a permanent
    /// [`AppError::Unprocessable`] when a cap is exceeded (spec §6.7 / §14).
    pub fn check(&self, tally: &ExtractTally) -> Result<(), AppError> {
        if tally.entries > self.max_entries {
            return Err(unprocessable("archive entry count exceeds cap"));
        }
        if tally.total_uncompressed > self.max_total_uncompressed {
            return Err(unprocessable("archive uncompressed size exceeds cap"));
        }
        if tally.max_entry_uncompressed > self.max_entry_uncompressed {
            return Err(unprocessable("archive entry size exceeds cap"));
        }
        if tally.depth > self.max_depth {
            return Err(unprocessable("archive nesting depth exceeds cap"));
        }
        if tally.compressed > 0 {
            let ratio = tally.total_uncompressed / tally.compressed.max(1);
            if ratio > self.max_ratio {
                return Err(unprocessable("archive compression ratio exceeds cap"));
            }
        }
        Ok(())
    }
}

fn unprocessable(msg: &str) -> AppError {
    AppError::Unprocessable {
        message: msg.to_owned(),
    }
}

/// Running tally accumulated while streaming an archive (spec §6.7).
#[derive(Debug, Clone, Default)]
pub struct ExtractTally {
    pub entries: u64,
    pub compressed: u64,
    pub total_uncompressed: u64,
    pub max_entry_uncompressed: u64,
    pub depth: u32,
}

/// Archive container kind, sniffed by magic bytes (`infer`), never by extension
/// (spec §12.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
}

/// `POST /api/v1/batch/submit` body (spec §11.2, scope: ingest).
#[derive(Debug, Clone, Deserialize, garde::Validate)]
pub struct SubmitBatchRequest {
    /// Path to an uploaded archive in the blob store, if submitting an archive.
    #[garde(skip)]
    pub archive_blob_key: Option<String>,
    /// Explicit path list, if submitting paths instead of an archive.
    #[garde(skip)]
    pub paths: Vec<String>,
    /// Failure threshold percentage before the job is marked PARTIAL_FAILURE
    /// (default 20, spec §6.7).
    #[garde(range(min = 0, max = 100))]
    #[serde(default = "default_threshold")]
    pub threshold_pct: i32,
}

fn default_threshold() -> i32 {
    20
}

/// Response to a batch submission: the job id + a status URL (spec §6.7).
#[derive(Debug, Clone, Serialize)]
pub struct SubmitBatchResponse {
    pub job_id: Uuid,
    pub total_units: i64,
    pub status_url: String,
}

/// Status of an in-flight or completed batch (spec §6.7).
#[derive(Debug, Clone, Serialize)]
pub struct BatchStatusResponse {
    pub job_id: Uuid,
    pub status: String,
    pub processed: i64,
    pub total: i64,
    pub failed_unit_count: i64,
    /// Estimated seconds remaining, if computable.
    pub eta_seconds: Option<i64>,
}

/// `POST /api/v1/batch/submit` — decompress (guarded), fan out work units.
pub async fn submit_batch(
    State(state): State<CoreState>,
    Json(req): Json<SubmitBatchRequest>,
) -> Result<Json<SubmitBatchResponse>, ApiError> {
    garde::Validate::validate(&req).map_err(|e| AppError::Validation {
        message: e.to_string(),
    })?;

    let job_id = new_id();

    // TODO:
    //   1. INSERT a jobs row (type=BATCH, status=PENDING, threshold_pct),
    //   2. if archive_blob_key: fetch from blob store and `expand_archive`
    //      (guarded by BombGuard) into per-file byte streams,
    //   3. for each file: sha256 + dedup + INSERT documents(PENDING) +
    //      work_units(QUEUED), then publish to JetStream (spec §6.2),
    //   4. UPDATE jobs.total_units; return immediately with the status URL.
    let guard = BombGuard::default();
    let _ = (&state.jetstream, guard, &req);

    Ok(Json(SubmitBatchResponse {
        job_id,
        total_units: 0,
        status_url: format!("/api/v1/batch/{job_id}/status"),
    }))
}

/// `GET /api/v1/batch/{job_id}/status` — progress + failure counts (spec §6.7).
pub async fn batch_status(
    State(state): State<CoreState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<BatchStatusResponse>, ApiError> {
    // TODO: SELECT status, processed_count, total_units, failed_unit_count FROM
    //       jobs WHERE id=$1 AND tenant_id=$2 (server-derived tenant, spec §12.7);
    //       404 via AppError::NotFound when absent. Compute ETA from recent rate.
    let _ = &state.db;
    Err(AppError::NotFound {
        resource: format!("job {job_id}"),
    }
    .into())
}

/// Stream-expand an archive, enforcing the [`BombGuard`] as the tally grows.
///
/// Returns the per-file (relative path, bytes) pairs on success, or a permanent
/// error the moment a cap is exceeded (spec §6.7 / §22 #6). Nested archives
/// recurse with `depth + 1`.
pub fn expand_archive(
    _kind: ArchiveKind,
    _data: &[u8],
    guard: &BombGuard,
    depth: u32,
) -> Result<Vec<(String, Vec<u8>)>, AppError> {
    let mut tally = ExtractTally {
        depth,
        ..Default::default()
    };
    // TODO: depending on kind, iterate entries with `zip::ZipArchive` /
    //       `tar::Archive` (over `flate2::read::GzDecoder` for TarGz). For each
    //       entry: update tally (entries, compressed, uncompressed, max entry,
    //       depth) and call guard.check(&tally) BEFORE allocating the output.
    //       Recurse into nested archives with depth+1.
    guard.check(&tally)?;
    let _ = &mut tally;
    Ok(Vec::new())
}

/// Thin wrapper over the NATS JetStream context used to publish work units
/// (spec §6.2). A stub for now; the real client lives here so handlers depend on
/// a stable type.
pub struct JetStreamPublisher {
    /// Stream name work units are published to.
    stream: String,
    // TODO: hold `async_nats::jetstream::Context` and a connection handle.
}

impl JetStreamPublisher {
    /// Connect to NATS and ensure the work-unit stream exists (spec §6.2).
    pub async fn connect(cfg: &NatsConfig) -> anyhow::Result<Self> {
        // TODO: async_nats::connect(&cfg.url).await -> jetstream::new(client);
        //       ensure the durable stream `cfg.stream` exists with the expected
        //       subjects + retention. Map errors via anyhow at the bin boundary.
        Ok(Self {
            stream: cfg.stream.clone(),
        })
    }

    /// Publish a single work unit; at-least-once delivery + worker idempotency =
    /// effectively-once (spec §6.2).
    pub async fn publish_work_unit(&self, _payload: &WorkUnitMessage) -> Result<(), AppError> {
        // TODO: serde_json-serialize the message, inject the X-Correlation-ID
        //       header, and `context.publish(subject, bytes).await?.await?` to
        //       confirm the ack (spec §6.2 / §13.1).
        let _ = &self.stream;
        Err(AppError::Dependency {
            dependency: "nats".to_owned(),
            message: "publish_work_unit not yet implemented".to_owned(),
        })
    }
}

/// The work-unit message published to NATS for `ingestion-worker` to consume
/// (spec §5.4 / §6.2). Idempotency key is `content_sha256`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkUnitMessage {
    pub work_unit_id: Uuid,
    pub job_id: Uuid,
    pub tenant_id: Uuid,
    pub document_ref: String,
    pub content_sha256: String,
    pub blob_key: String,
}
