#![forbid(unsafe_code)]
//! Typed, 12-factor configuration (spec §0, §19).
//!
//! Configuration is loaded from environment variables (prefix `DIYRAG_`) layered
//! over optional TOML defaults via [`figment`]. There are **no hardcoded hosts,
//! ports, secrets, or model names** anywhere in the codebase — everything that
//! varies between deployments flows through [`AppConfig`].
//!
//! DECISION: figment (env + toml providers) is chosen over plain `envy` because
//! it composes layered sources (file defaults → env overrides) and gives typed
//! deserialization with good error messages. (Reversible: swap the provider stack
//! without changing [`AppConfig`].)

use serde::{Deserialize, Serialize};

use crate::errors::AppError;

/// The environment-variable prefix for every diyRAG setting, e.g.
/// `DIYRAG_HTTP__BIND_ADDR`, `DIYRAG_DATABASE__URL`.
pub const ENV_PREFIX: &str = "DIYRAG_";

/// Top-level application configuration shared by all services.
///
/// Each service reads the whole struct and uses the sub-sections it needs; this
/// keeps a single source of truth and one `.env.example` (spec §17).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    /// Logical name of the running service, surfaced in logs/traces and the
    /// `error_log.service_name` column (spec §13.2).
    pub service_name: String,
    /// Deployment environment label, e.g. `dev`, `staging`, `prod`.
    #[serde(default = "default_environment")]
    pub environment: String,
    /// HTTP listener configuration.
    pub http: HttpConfig,
    /// PostgreSQL connection settings (spec §5.1).
    pub database: DatabaseConfig,
    /// Qdrant vector-store settings (spec §5.2).
    pub qdrant: QdrantConfig,
    /// Blob / object-store settings (spec §5.3).
    pub blob: BlobConfig,
    /// NATS JetStream broker settings (spec §6.2).
    pub nats: NatsConfig,
    /// Observability / logging settings (spec §13).
    #[serde(default)]
    pub observability: ObservabilityConfig,
    /// Auth / JWT settings (spec §12.2).
    pub auth: AuthConfig,
}

/// HTTP server binding + limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpConfig {
    /// `host:port` to bind, e.g. `0.0.0.0:8080`.
    pub bind_addr: String,
    /// Maximum request body size in bytes (Tower limit, spec §12.3).
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    /// Comma-separated CORS allow-list of origins; empty = deny all cross-origin.
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
}

/// PostgreSQL settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DatabaseConfig {
    /// libpq-style DSN. NOTE: contains a secret — never log it.
    pub url: String,
    /// Maximum pool connections.
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
    /// Whether to run pending migrations on startup.
    #[serde(default)]
    pub run_migrations_on_start: bool,
}

/// Qdrant settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QdrantConfig {
    /// Qdrant gRPC/HTTP endpoint URL.
    pub url: String,
    /// Optional API key (secret — never log).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Collection name prefix; collections are `{prefix}{tenant_slug}` (spec §5.2).
    #[serde(default = "default_collection_prefix")]
    pub collection_prefix: String,
}

/// Blob / object-store settings (spec §5.3).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlobConfig {
    /// Backend selector, e.g. `s3`, `local`, `memory`.
    pub backend: String,
    /// Bucket / container / root path depending on the backend.
    pub bucket: String,
    /// Optional endpoint override (e.g. MinIO).
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Optional region for S3-compatible backends.
    #[serde(default)]
    pub region: Option<String>,
}

/// NATS JetStream settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NatsConfig {
    /// NATS server URL(s), comma-separated.
    pub url: String,
    /// JetStream stream name for ingestion work units.
    #[serde(default = "default_stream")]
    pub stream: String,
}

/// Observability settings (spec §13).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObservabilityConfig {
    /// `tracing` `EnvFilter` directive, e.g. `info,diyrag_core_api=debug`.
    #[serde(default = "default_log_filter")]
    pub log_filter: String,
    /// Emit JSON logs (true in prod) vs. pretty logs (false in dev).
    #[serde(default = "default_true")]
    pub json_logs: bool,
    /// Optional OTLP exporter endpoint for traces.
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_filter: default_log_filter(),
            json_logs: true,
            otlp_endpoint: None,
        }
    }
}

/// Auth settings (spec §12.2).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthConfig {
    /// JWT issuer expected on inbound tokens.
    pub jwt_issuer: String,
    /// JWT audience expected on inbound tokens.
    pub jwt_audience: String,
    /// Path to the PEM-encoded public key / JWKS used to verify JWTs.
    /// NOTE: the *private* key never lives here; verification only.
    pub jwt_public_key_path: String,
}

impl AppConfig {
    /// Load configuration from environment variables (and optional TOML defaults).
    ///
    /// `defaults_toml` is an optional path to a TOML file providing base values;
    /// environment variables (prefix [`ENV_PREFIX`], `__` as the nesting separator)
    /// always win.
    pub fn load(defaults_toml: Option<&str>) -> Result<Self, AppError> {
        // TODO: build the figment provider stack:
        //   1. optional `Toml::file(defaults_toml)` for base values,
        //   2. `Env::prefixed(ENV_PREFIX).split("__")` for overrides,
        //   then `.extract::<AppConfig>()`. Map figment errors into
        //   AppError::Config { .. }. Reference `defaults_toml` here so the
        //   signature is honored once implemented.
        let _ = defaults_toml;
        Err(AppError::Config {
            message: "AppConfig::load not yet implemented".to_owned(),
        })
    }

    /// Parse [`HttpConfig::bind_addr`] into a [`std::net::SocketAddr`].
    pub fn socket_addr(&self) -> Result<std::net::SocketAddr, AppError> {
        self.http
            .bind_addr
            .parse()
            .map_err(|e: std::net::AddrParseError| AppError::Config {
                message: format!("invalid bind_addr `{}`: {e}", self.http.bind_addr),
            })
    }
}

fn default_environment() -> String {
    "dev".to_owned()
}
fn default_max_body_bytes() -> usize {
    // 16 MiB default request cap; per-endpoint overrides live in the gateway.
    16 * 1024 * 1024
}
fn default_db_max_connections() -> u32 {
    16
}
fn default_collection_prefix() -> String {
    "t_".to_owned()
}
fn default_stream() -> String {
    "diyrag-ingest".to_owned()
}
fn default_log_filter() -> String {
    "info".to_owned()
}
fn default_true() -> bool {
    true
}
