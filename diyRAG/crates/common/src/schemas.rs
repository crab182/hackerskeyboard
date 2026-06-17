#![forbid(unsafe_code)]
//! Storage-layer schemas (spec §5.1).
//!
//! One `#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize)]` struct
//! per PostgreSQL table, plus the status/retention/structure enums. The schema
//! is the integration contract for LAN sync (spec §5), so these types mirror the
//! columns exactly.
//!
//! Conventions:
//! - All PKs are UUIDv7 ([`uuid::Uuid`], generated via [`crate::ids`]).
//! - All timestamps are UTC ([`chrono::DateTime<Utc>`]).
//! - JSONB columns map to `sqlx::types::Json<T>`.
//! - Postgres enums map to Rust enums with `#[sqlx(type_name, rename_all)]`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::{DomainScope, Role, Scope};

// ---------------------------------------------------------------------------
// Enums (Postgres enum types, spec §5.1)
// ---------------------------------------------------------------------------

/// Document processing status (spec §5.1 `documents.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "document_status", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum DocumentStatus {
    Pending,
    Parsing,
    Chunking,
    Embedding,
    Indexed,
    Quarantined,
}

/// Content-retention status (spec §5.1, §6.6). Query layer always filters `ACTIVE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "retention_status", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum RetentionStatus {
    Active,
    PurgedLogical,
}

/// Chunk structure type (spec §5.1 `chunks.structure_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "structure_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum StructureType {
    Prose,
    Table,
    Heading,
    Code,
    Triple,
}

/// User account status (spec §5.1 `users.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "user_status", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum UserStatus {
    Active,
    Suspended,
    Disabled,
}

/// Job type (spec §5.1 `jobs.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "job_type", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum JobType {
    Batch,
    Reindex,
    Sync,
}

/// Job status (spec §5.1 `jobs.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "job_status", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum JobStatus {
    Pending,
    Running,
    Complete,
    Failed,
    PartialFailure,
}

/// Work-unit state (spec §5.1 `work_units.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "work_unit_state", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum WorkUnitState {
    Queued,
    InProgress,
    Success,
    Failure,
    FailedRecoverable,
    Dlq,
}

/// Error-log severity level (spec §13.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "log_level", rename_all = "UPPERCASE")]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Critical,
}

/// A `{node: counter}` version vector for CRDT registry sync (spec §9).
pub type VersionVector = std::collections::BTreeMap<String, i64>;

// ---------------------------------------------------------------------------
// Table structs (spec §5.1)
// ---------------------------------------------------------------------------

/// `tenants` — isolation boundary (spec §5.1).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}

/// `users` (spec §5.1).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub status: UserStatus,
    pub created_at: DateTime<Utc>,
}

/// `api_keys` — only the argon2 hash is stored, never the raw key (spec §5.1, §12.2).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    /// Argon2 PHC hash. NOTE: secret-adjacent; never expose over the API.
    #[serde(skip_serializing)]
    pub key_hash: String,
    /// Short non-secret prefix shown in the UI.
    pub prefix: String,
    /// Resource scopes (JSONB).
    pub scopes: Json<Vec<Scope>>,
    /// Domain scope: which collections/roots the key may touch (JSONB).
    pub domain_scope: Json<DomainScope>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// `roots` — watched folder roots (spec §5.1, §6.1).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Root {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub path: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub watch: bool,
    pub include_globs: Json<Vec<String>>,
    pub exclude_globs: Json<Vec<String>>,
    pub source_root_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// `documents` — UNIQUE `(tenant_id, content_sha256)` enforces dedup (spec §5.1).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub root_id: Option<Uuid>,
    pub source_path: String,
    pub content_sha256: String,
    pub mime: String,
    pub bytes: i64,
    pub parser: String,
    pub status: DocumentStatus,
    pub retention_status: RetentionStatus,
    pub version_vector: Json<VersionVector>,
    pub lang: Option<String>,
    pub page_count: Option<i32>,
    pub error_ref: Option<Uuid>,
    pub blob_key: String,
    pub created_at: DateTime<Utc>,
    pub indexed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// `chunks` — `vector_id` mirrors the Qdrant point id (spec §5.1, §5.2).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Chunk {
    pub id: Uuid,
    pub document_id: Uuid,
    pub tenant_id: Uuid,
    pub ordinal: i32,
    pub text: String,
    pub token_count: i32,
    pub section_heading: Option<String>,
    pub page_number: Option<i32>,
    pub structure_type: StructureType,
    pub embed_model: String,
    pub vector_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// `jobs` (spec §5.1, §6.7).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub tenant_id: Uuid,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub job_type: JobType,
    pub status: JobStatus,
    pub total_units: i64,
    pub processed_count: i64,
    pub failed_unit_count: i64,
    pub threshold_pct: i32,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// `work_units` — idempotent unit keyed by `content_sha256` (spec §5.1, §6.2).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WorkUnit {
    pub id: Uuid,
    pub job_id: Uuid,
    pub document_ref: String,
    pub content_sha256: String,
    pub state: WorkUnitState,
    pub retry_count: i32,
    pub last_error_ref: Option<Uuid>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
}

/// `error_log` — append-only, monthly partitions; `log_id` is the reference code
/// surfaced in UIs (spec §13.2).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ErrorLog {
    pub log_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub service_name: String,
    pub user_id: Option<String>,
    pub api_key_id: Option<String>,
    pub correlation_id: Uuid,
    pub transaction_id: Option<String>,
    pub message: String,
    pub stack_trace: Option<Json<serde_json::Value>>,
    /// Request params, PII-scrubbed (spec §13.2).
    pub context: Option<Json<serde_json::Value>>,
}

/// `audit_log` — append-only record of privileged actions (spec §5.1, §12.9).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub actor_user_id: Option<Uuid>,
    pub actor_key_id: Option<Uuid>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub before: Option<Json<serde_json::Value>>,
    pub after: Option<Json<serde_json::Value>>,
    pub ip: Option<String>,
    pub correlation_id: Uuid,
    pub at: DateTime<Utc>,
}

/// `sync_state` — LAN sync registry record (spec §5.1, §9).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SyncState {
    pub record_key: String,
    pub tenant_id: Uuid,
    pub version_vector: Json<VersionVector>,
    pub last_hash: String,
    pub updated_at: DateTime<Utc>,
    pub origin_node: String,
}

/// `nodes` — known LAN peers with cert pinning (spec §5.1, §9).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Node {
    pub id: Uuid,
    pub name: String,
    pub priority: i32,
    pub last_seen: Option<DateTime<Utc>>,
    pub cert_fingerprint: String,
    pub endpoint: String,
}

/// `roles` + `user_roles` are modeled at the auth layer as [`Role`]; this struct
/// represents a row of the `roles` lookup table for admin RBAC management (§12.6).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RoleRow {
    pub id: Uuid,
    pub name: Role,
}
