#![forbid(unsafe_code)]
//! `diyrag-common` — the shared library for the diyRAG platform.
//!
//! Every service (api-gateway, core-api, retrieval, ingestion-worker, sync-agent,
//! mcp-server, autoscaler, diyragd) depends on this crate for:
//!
//! - [`config`]      — typed, env-driven configuration ([`config::AppConfig`]).
//! - [`logging`]     — `tracing` JSON subscriber + correlation-id span layer.
//! - [`correlation`] — the [`correlation::CorrelationId`] newtype + Axum extractor.
//! - [`errors`]      — the [`errors::AppError`] enum, [`errors::Classification`],
//!                     and the standard error envelope (spec §11.3).
//! - [`auth`]        — API-key hashing/verification (argon2), JWT verify, scopes,
//!                     domain scopes, and the RBAC [`auth::Role`] enum.
//! - [`db`]          — `sqlx` `PgPool` initialization + migration helper.
//! - [`schemas`]     — `serde` + `sqlx::FromRow` structs for the §5.1 tables.
//! - [`vector`]      — the [`vector::VectorStore`] trait + a Qdrant impl skeleton.
//! - [`blob`]        — an `object_store` wrapper + content-addressed key helper.
//! - [`ids`]         — UUIDv7 helpers (sortable primary keys, spec §5.1).
//!
//! The crate uses `thiserror` (never `anyhow`) per the coding standards in §19.

pub mod auth;
pub mod blob;
pub mod config;
pub mod correlation;
pub mod db;
pub mod errors;
pub mod ids;
pub mod logging;
pub mod schemas;
pub mod vector;

/// Crate-wide convenience result type bound to [`errors::AppError`].
pub type Result<T> = std::result::Result<T, errors::AppError>;

/// Re-export the most commonly used items so callers can `use diyrag_common::prelude::*`.
pub mod prelude {
    pub use crate::config::AppConfig;
    pub use crate::correlation::CorrelationId;
    pub use crate::errors::{AppError, Classification, ErrorEnvelope};
    pub use crate::ids::new_id;
    pub use crate::Result;
}
