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
        use figment::providers::{Env, Format, Toml};
        use figment::Figment;

        let mut figment = Figment::new();
        // 1. Optional TOML base layer. A missing file is non-fatal — env may
        //    supply every value (12-factor; spec §0). `Toml::file` is lenient
        //    about absence by design.
        if let Some(path) = defaults_toml {
            figment = figment.merge(Toml::file(path));
        }
        // 2. Environment overrides ALWAYS win. `DIYRAG_HTTP__BIND_ADDR` maps to
        //    `http.bind_addr` (prefix stripped, `__` is the nesting separator).
        figment = figment.merge(Env::prefixed(ENV_PREFIX).split("__"));

        figment
            .extract::<AppConfig>()
            .map_err(|e| AppError::Config {
                message: e.to_string(),
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

#[cfg(test)]
mod tests {
    // `figment::Jail::expect_with` requires the closure to return
    // `Result<(), figment::Error>`; `figment::Error` is large, which trips
    // `clippy::result_large_err` on the harness closures (test-only — it is
    // figment's API surface, not ours).
    #![allow(clippy::result_large_err)]

    use super::*;

    /// Set the minimum required (no-default) fields so `extract` succeeds.
    fn set_required(jail: &mut figment::Jail) {
        jail.set_env("DIYRAG_SERVICE_NAME", "api-gateway");
        jail.set_env("DIYRAG_HTTP__BIND_ADDR", "127.0.0.1:8443");
        jail.set_env("DIYRAG_DATABASE__URL", "postgres://u:p@db:5432/diyrag");
        jail.set_env("DIYRAG_QDRANT__URL", "http://qdrant:6333");
        jail.set_env("DIYRAG_BLOB__BACKEND", "local");
        jail.set_env("DIYRAG_BLOB__BUCKET", "diyrag-blobs");
        jail.set_env("DIYRAG_NATS__URL", "nats://nats:4222");
        jail.set_env("DIYRAG_AUTH__JWT_ISSUER", "https://idp.lan/realms/diyrag");
        jail.set_env("DIYRAG_AUTH__JWT_AUDIENCE", "diyrag-api");
        jail.set_env("DIYRAG_AUTH__JWT_PUBLIC_KEY_PATH", "/etc/diyrag/jwt.pem");
    }

    #[test]
    fn env_loads_with_serde_defaults_for_unset_fields() {
        figment::Jail::expect_with(|jail| {
            set_required(jail);
            let cfg = AppConfig::load(None).expect("config should load from env");
            assert_eq!(cfg.service_name, "api-gateway");
            assert_eq!(cfg.http.bind_addr, "127.0.0.1:8443");
            assert_eq!(cfg.database.url, "postgres://u:p@db:5432/diyrag");
            // Defaults fill everything not provided:
            assert_eq!(cfg.environment, "dev");
            assert_eq!(cfg.database.max_connections, 16);
            assert!(!cfg.database.run_migrations_on_start);
            assert_eq!(cfg.qdrant.collection_prefix, "t_");
            assert_eq!(cfg.nats.stream, "diyrag-ingest");
            assert_eq!(cfg.http.max_body_bytes, 16 * 1024 * 1024);
            assert!(cfg.observability.json_logs);
            assert!(cfg.http.cors_allowed_origins.is_empty());
            Ok(())
        });
    }

    #[test]
    fn env_overrides_win_over_defaults() {
        figment::Jail::expect_with(|jail| {
            set_required(jail);
            jail.set_env("DIYRAG_ENVIRONMENT", "prod");
            jail.set_env("DIYRAG_DATABASE__MAX_CONNECTIONS", "42");
            jail.set_env("DIYRAG_DATABASE__RUN_MIGRATIONS_ON_START", "true");
            jail.set_env("DIYRAG_OBSERVABILITY__JSON_LOGS", "false");
            let cfg = AppConfig::load(None).unwrap();
            assert_eq!(cfg.environment, "prod");
            assert_eq!(cfg.database.max_connections, 42);
            assert!(cfg.database.run_migrations_on_start);
            assert!(!cfg.observability.json_logs);
            Ok(())
        });
    }

    #[test]
    fn missing_required_field_is_a_config_error() {
        figment::Jail::expect_with(|jail| {
            // Only service_name present; the rest are absent → extract must fail,
            // surfacing AppError::Config (never a panic).
            jail.set_env("DIYRAG_SERVICE_NAME", "x");
            let err = AppConfig::load(None).unwrap_err();
            assert!(matches!(err, AppError::Config { .. }), "got {err:?}");
            Ok(())
        });
    }

    #[test]
    fn socket_addr_parses_then_rejects_garbage() {
        figment::Jail::expect_with(|jail| {
            set_required(jail);
            let cfg = AppConfig::load(None).unwrap();
            let sa = cfg.socket_addr().expect("valid bind_addr");
            assert_eq!(sa.port(), 8443);
            assert!(sa.ip().is_loopback());

            jail.set_env("DIYRAG_HTTP__BIND_ADDR", "not-an-addr");
            let bad = AppConfig::load(None).unwrap();
            assert!(matches!(bad.socket_addr(), Err(AppError::Config { .. })));
            Ok(())
        });
    }
}
