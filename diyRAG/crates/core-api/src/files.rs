#![forbid(unsafe_code)]
//! File & folder-root management + the watched-root file watcher (spec §6.1).
//!
//! Roots are registered/deregistered via REST; `watch = true` roots are observed
//! with the [`notify`] crate, honoring per-root include/exclude globs
//! ([`globset`]), debounced, with new/changed files enqueued **by
//! `content_sha256`, not mtime** (spec §6.1). Deleted source files set
//! `retention_status = PURGED_LOGICAL` (logical, reversible) — never a hard
//! delete by default (spec §6.1 / §6.6).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use diyrag_common::errors::AppError;
use diyrag_common::ids::new_id;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::CoreState;

/// Default debounce window for the watcher (spec §6.1 "debounced").
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Request body for `POST /api/v1/files/roots` (spec §11.2, scope: ingest).
#[derive(Debug, Clone, Deserialize, garde::Validate)]
pub struct RegisterRootRequest {
    /// Absolute filesystem path to watch/ingest.
    #[garde(length(min = 1, max = 4096))]
    pub path: String,
    /// Optional human description.
    #[garde(length(max = 1024))]
    pub description: Option<String>,
    /// Whether to attach a live file watcher (spec §6.1).
    #[garde(skip)]
    pub watch: bool,
    /// Include globs; empty = include everything not excluded.
    #[garde(skip)]
    pub include_globs: Vec<String>,
    /// Exclude globs (applied after include).
    #[garde(skip)]
    pub exclude_globs: Vec<String>,
}

/// Response for a registered root.
#[derive(Debug, Clone, Serialize)]
pub struct RootResponse {
    pub root_id: Uuid,
    pub path: String,
    pub watch: bool,
}

/// Request body for `POST /api/v1/ingestion/trigger` (spec §11.2, scope: ingest).
#[derive(Debug, Clone, Deserialize, garde::Validate)]
pub struct TriggerIngestRequest {
    /// Specific paths to (re)ingest on demand.
    #[garde(length(min = 1))]
    pub paths: Vec<String>,
    /// Optional root the paths belong to.
    #[garde(skip)]
    pub root_id: Option<Uuid>,
}

/// `POST /api/v1/files/roots` — register a watched/ingested root (spec §6.1).
///
/// Tenant id is derived server-side from the authenticated principal forwarded by
/// the gateway (spec §12.7); it is never read from the request body.
pub async fn register_root(
    State(state): State<CoreState>,
    Json(req): Json<RegisterRootRequest>,
) -> Result<Json<RootResponse>, super::rag::ApiError> {
    garde::Validate::validate(&req).map_err(|e| AppError::Validation {
        message: e.to_string(),
    })?;

    let root_id = new_id();
    // Validate glob sets early so a malformed pattern fails the request, not the
    // watcher task (spec §12.4 whitelist validation).
    let _include = build_globset(&req.include_globs)?;
    let _exclude = build_globset(&req.exclude_globs)?;

    // TODO: INSERT INTO roots (id, tenant_id, path, description, is_active=true,
    //       watch, include_globs, exclude_globs, created_at) VALUES (...)
    //       parameterized (spec §12.4). Write an audit_log entry. If watch=true,
    //       signal the running watcher to (re)subscribe to this root.
    let _ = &state.db;

    Ok(Json(RootResponse {
        root_id,
        path: req.path,
        watch: req.watch,
    }))
}

/// `POST /api/v1/ingestion/trigger` — enqueue specific paths for ingestion.
pub async fn trigger_ingest(
    State(state): State<CoreState>,
    Json(req): Json<TriggerIngestRequest>,
) -> Result<Json<EnqueueSummary>, super::rag::ApiError> {
    garde::Validate::validate(&req).map_err(|e| AppError::Validation {
        message: e.to_string(),
    })?;

    let mut enqueued = 0usize;
    for path in &req.paths {
        // TODO: hash the file (content_sha256), dedup against documents
        //       (tenant_id, content_sha256), and publish a work unit if new
        //       (spec §6.2). Count successes.
        if enqueue_path(&state, Path::new(path), req.root_id).await.is_ok() {
            enqueued += 1;
        }
    }

    Ok(Json(EnqueueSummary {
        enqueued,
        requested: req.paths.len(),
    }))
}

/// Summary returned by an enqueue operation.
#[derive(Debug, Clone, Serialize)]
pub struct EnqueueSummary {
    pub enqueued: usize,
    pub requested: usize,
}

/// Build a [`GlobSet`] from a list of patterns; an empty list yields `None`-like
/// "match everything" semantics handled by [`RootMatcher`].
fn build_globset(patterns: &[String]) -> Result<GlobSet, AppError> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).map_err(|e| AppError::Validation {
            message: format!("invalid glob `{p}`: {e}"),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| AppError::Validation {
        message: format!("invalid glob set: {e}"),
    })
}

/// Per-root include/exclude matcher (spec §6.1).
pub struct RootMatcher {
    include: GlobSet,
    exclude: GlobSet,
    /// True when no include patterns were given (include-everything default).
    include_all: bool,
}

impl RootMatcher {
    /// Construct from raw pattern lists.
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, AppError> {
        Ok(Self {
            include: build_globset(include)?,
            exclude: build_globset(exclude)?,
            include_all: include.is_empty(),
        })
    }

    /// Should this path be ingested? Include first, then exclude (spec §6.1).
    #[must_use]
    pub fn matches(&self, path: &Path) -> bool {
        let included = self.include_all || self.include.is_match(path);
        included && !self.exclude.is_match(path)
    }
}

/// Long-running watcher task for all `watch = true` roots (spec §6.1).
///
/// Uses [`notify`] to observe each active root, applies the [`RootMatcher`],
/// debounces rapid events, and enqueues new/changed files keyed by
/// `content_sha256`. Runs until the process shuts down.
pub async fn run_watcher(state: CoreState) -> Result<(), AppError> {
    // TODO: load all active watch=true roots from Postgres into a
    //       HashMap<Uuid, (PathBuf, RootMatcher)>; subscribe to a control channel
    //       so register_root/deactivate_root can add/remove watches at runtime.
    let watched: HashMap<Uuid, (PathBuf, RootMatcher)> = HashMap::new();

    // notify is callback/std-channel based; bridge it to async with an mpsc.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DebouncedEvent>(1024);

    // TODO: build a notify::RecommendedWatcher whose handler coalesces events
    //       within DEBOUNCE and forwards DebouncedEvent on `tx`. Add each root
    //       path with RecursiveMode::Recursive.
    let _ = (&tx, DEBOUNCE);

    tracing::info!(roots = watched.len(), "file watcher started");

    while let Some(event) = rx.recv().await {
        match event.kind {
            EventKind::CreatedOrModified => {
                // Resolve which root owns this path, apply its matcher, then
                // enqueue by content hash if it changed (spec §6.1).
                if let Some((root_id, matcher_path)) = owning_root(&watched, &event.path) {
                    let (_, matcher) = &watched[&root_id];
                    if matcher.matches(&event.path) {
                        let _ = enqueue_path(&state, &event.path, Some(root_id)).await;
                    }
                    let _ = matcher_path;
                }
            }
            EventKind::Removed => {
                // Logical purge, never a hard delete (spec §6.1 / §6.6).
                if let Some((root_id, _)) = owning_root(&watched, &event.path) {
                    let _ = logical_purge_path(&state, &event.path, root_id).await;
                }
            }
        }
    }

    Ok(())
}

/// A debounced filesystem event normalized off the raw `notify` stream.
#[derive(Debug, Clone)]
pub struct DebouncedEvent {
    pub path: PathBuf,
    pub kind: EventKind,
}

/// The subset of filesystem change kinds the watcher acts on (spec §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// File created or its contents modified — candidate for (re)ingestion.
    CreatedOrModified,
    /// Source file removed — triggers logical purge (spec §6.6).
    Removed,
}

/// Find the root that owns `path` (longest-prefix match over root paths).
fn owning_root(
    watched: &HashMap<Uuid, (PathBuf, RootMatcher)>,
    path: &Path,
) -> Option<(Uuid, PathBuf)> {
    watched
        .iter()
        .filter(|(_, (root_path, _))| path.starts_with(root_path))
        .max_by_key(|(_, (root_path, _))| root_path.as_os_str().len())
        .map(|(id, (root_path, _))| (*id, root_path.clone()))
}

/// Hash a file and, if new for `(tenant, content_sha256)`, publish a work unit
/// to NATS (spec §6.2 idempotency). Returns Ok even when the file already exists
/// (it is simply not re-enqueued).
async fn enqueue_path(state: &CoreState, path: &Path, root_id: Option<Uuid>) -> Result<(), AppError> {
    // TODO:
    //   1. stream-hash the file with sha2::Sha256 -> content_sha256,
    //   2. SELECT 1 FROM documents WHERE tenant_id=$1 AND content_sha256=$2
    //      AND status='INDEXED' (dedup, spec §6.2); if present, return Ok(()),
    //   3. INSERT a PENDING documents row (or reuse) + a work_unit (QUEUED),
    //   4. publish the work unit on JetStream via state.jetstream (spec §6.2).
    let _ = (state, path, root_id);
    Ok(())
}

/// Set `retention_status = PURGED_LOGICAL` for the document at `path` (spec §6.6).
/// Reversible and audited; data + blobs are retained.
async fn logical_purge_path(state: &CoreState, path: &Path, root_id: Uuid) -> Result<(), AppError> {
    // TODO: UPDATE documents SET retention_status='PURGED_LOGICAL', updated_at=now()
    //       WHERE tenant_id=$1 AND root_id=$2 AND source_path=$3; mirror onto
    //       Qdrant payload; write an audit_log entry (spec §6.6).
    let _ = (state, path, root_id);
    Ok(())
}
