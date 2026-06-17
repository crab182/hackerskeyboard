#![forbid(unsafe_code)]
//! Error model (spec §11.3 envelope + §14 taxonomy).
//!
//! - [`AppError`] is the library error enum (`thiserror`, never `anyhow` in libs).
//! - [`Classification`] tags each error `Transient` (retry w/ backoff) or
//!   `Permanent` (quarantine) per §14.
//! - [`ErrorEnvelope`] is the exact serde-serialized shape returned to clients
//!   (§11.3); every failed API response uses it and carries a `reference_code`
//!   that deep-links to the matching `error_log` row.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::correlation::CorrelationId;

/// Retry classification for the recovery machinery (spec §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Classification {
    /// Network/timeout/GPU-OOM-class failures: retry with exponential backoff.
    Transient,
    /// Corrupt/unsupported/logic failures: route straight to quarantine.
    Permanent,
}

/// The platform-wide error type used throughout the library and surfaced to
/// services. Each variant maps to a stable `error_id` and a [`Classification`].
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Configuration could not be loaded or validated.
    #[error("configuration error: {message}")]
    Config { message: String },

    /// Authentication failed (bad/expired key or token).
    #[error("authentication failed: {message}")]
    Unauthorized { message: String },

    /// Authenticated but missing the required scope/role (spec §12.6).
    #[error("forbidden: {message}")]
    Forbidden { message: String },

    /// Request payload failed schema/whitelist validation (spec §12.4).
    #[error("invalid request: {message}")]
    Validation { message: String },

    /// Requested resource does not exist (or is not visible to the caller).
    #[error("not found: {resource}")]
    NotFound { resource: String },

    /// A downstream dependency (db/qdrant/blob/nats/gpu) failed transiently.
    #[error("dependency `{dependency}` unavailable: {message}")]
    Dependency {
        dependency: String,
        message: String,
    },

    /// Database access error.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Document/content could not be processed and is permanently rejected.
    #[error("unprocessable content: {message}")]
    Unprocessable { message: String },

    /// Catch-all internal error.
    #[error("internal error: {message}")]
    Internal { message: String },
}

impl AppError {
    /// Classify this error for the retry/quarantine decision (spec §14).
    #[must_use]
    pub fn classification(&self) -> Classification {
        match self {
            // Transient: worth a backoff retry.
            AppError::Dependency { .. } => Classification::Transient,
            AppError::Database(e) if is_transient_db_error(e) => Classification::Transient,
            // Everything else is a permanent/logic failure.
            _ => Classification::Permanent,
        }
    }

    /// Stable machine-readable error id (e.g. `E403_USER_PERMS`) used in the
    /// envelope and matched in dashboards.
    #[must_use]
    pub fn error_id(&self) -> &'static str {
        match self {
            AppError::Config { .. } => "E500_CONFIG",
            AppError::Unauthorized { .. } => "E401_UNAUTHORIZED",
            AppError::Forbidden { .. } => "E403_USER_PERMS",
            AppError::Validation { .. } => "E422_VALIDATION",
            AppError::NotFound { .. } => "E404_NOT_FOUND",
            AppError::Dependency { .. } => "E503_DEPENDENCY",
            AppError::Database(_) => "E500_DATABASE",
            AppError::Unprocessable { .. } => "E422_UNPROCESSABLE",
            AppError::Internal { .. } => "E500_INTERNAL",
        }
    }

    /// HTTP status code this error maps to at the API edge.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            AppError::Unauthorized { .. } => 401,
            AppError::Forbidden { .. } => 403,
            AppError::Validation { .. } | AppError::Unprocessable { .. } => 422,
            AppError::NotFound { .. } => 404,
            AppError::Dependency { .. } => 503,
            AppError::Config { .. } | AppError::Database(_) | AppError::Internal { .. } => 500,
        }
    }

    /// A plain-language message safe to show non-admin users (spec §10.4).
    /// Never leaks stack traces or internal detail.
    #[must_use]
    pub fn user_facing_message(&self) -> String {
        match self {
            AppError::Unauthorized { .. } => "Authentication is required or your credentials are invalid.".to_owned(),
            AppError::Forbidden { .. } => "Access denied. You do not have permission to perform this action.".to_owned(),
            AppError::Validation { .. } => "The request could not be processed because it was malformed.".to_owned(),
            AppError::NotFound { resource } => format!("The requested {resource} could not be found."),
            AppError::Unprocessable { .. } => "This content could not be processed.".to_owned(),
            AppError::Dependency { .. } => "A backend service is temporarily unavailable. Please try again shortly.".to_owned(),
            _ => "An unexpected error occurred. The reference code can help support investigate.".to_owned(),
        }
    }

    /// Build the standard client-facing envelope (spec §11.3).
    ///
    /// `reference_code` should equal the `error_log.log_id` written for this
    /// failure so the GUI can deep-link to it.
    #[must_use]
    pub fn to_envelope(
        &self,
        reference_code: impl Into<String>,
        correlation_id: CorrelationId,
    ) -> ErrorEnvelope {
        let reference_code = reference_code.into();
        ErrorEnvelope {
            success: false,
            error_id: self.error_id().to_owned(),
            user_facing_message: self.user_facing_message(),
            technical_details: self.to_string(),
            suggestion_link: format!("/app/errors?ref={reference_code}"),
            reference_code,
            correlation_id: correlation_id.to_string(),
            timestamp: Utc::now(),
        }
    }
}

/// The standard error envelope returned for *all* failures (spec §11.3).
///
/// Field order/names match the spec's JSON exactly so the GUI and clients can
/// rely on a single shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// Always `false` on this struct.
    pub success: bool,
    /// Stable machine id, e.g. `E403_USER_PERMS`.
    pub error_id: String,
    /// Plain-language message for end users.
    pub user_facing_message: String,
    /// Technical detail (shown to admins; PII-scrubbed).
    pub technical_details: String,
    /// Clickable reference code; equals `error_log.log_id`.
    pub reference_code: String,
    /// The request correlation id (spec §13.1).
    pub correlation_id: String,
    /// When the error was emitted (UTC).
    pub timestamp: DateTime<Utc>,
    /// Deep-link into the Errors/Debug screen (spec §10.4).
    pub suggestion_link: String,
}

/// Heuristic: is this a transient sqlx error worth retrying?
fn is_transient_db_error(err: &sqlx::Error) -> bool {
    // TODO: refine — treat pool timeouts and connection drops as transient,
    // constraint violations / decode errors as permanent. For now, only
    // explicit pool timeouts and IO are transient.
    matches!(
        err,
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_)
    )
}
