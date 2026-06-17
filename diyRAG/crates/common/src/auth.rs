#![forbid(unsafe_code)]
//! Authentication & authorization primitives (spec §12.2, §12.6).
//!
//! **Security controls are deterministic code OUTSIDE the LLM** (spec §0/§12).
//! This module provides:
//! - API-key hashing/verification with **argon2** (raw keys are never stored;
//!   only a salted hash, spec §12.2).
//! - JWT verification stubs (spec §12.2).
//! - Resource [`Scope`]s and [`DomainScope`] (which collections/roots a key may
//!   touch, spec §5.1 `api_keys.scopes` / `domain_scope`).
//! - The RBAC [`Role`] enum (Reader/Ingester/Admin, spec §12.6).
//!
//! All access-control decisions are pure functions over an [`AuthContext`];
//! the tenant id is always derived from the authenticated principal, never from
//! client input (spec §12.7).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::AppError;

/// RBAC roles, least-privilege first (spec §12.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Role {
    /// `read_vector`, `read_metadata`, `query`.
    Reader,
    /// Reader + `add_files`, `process_upload`, `write:ingest_logs`.
    Ingester,
    /// All capabilities + user/key/node management, config, full audit, service mgmt.
    Admin,
}

impl Role {
    /// Does this role satisfy a required minimum role? (Ordering: Reader < Ingester < Admin.)
    #[must_use]
    pub fn satisfies(self, required: Role) -> bool {
        self >= required
    }
}

/// A resource permission scope attached to an API key (spec §5.1 `scopes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Read/query the corpus.
    Reader,
    /// Add files / register roots.
    Ingest,
    /// Privileged administrative operations.
    Admin,
    /// An extension scope not in the baseline set.
    Other(String),
}

/// Which collections/roots a key may touch (spec §5.1 `domain_scope`).
///
/// An empty `roots`/`collections` set with `all = true` means unrestricted
/// within the tenant; otherwise access is limited to the listed ids.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainScope {
    /// If true, the key may touch every root/collection in its tenant.
    #[serde(default)]
    pub all: bool,
    /// Explicitly allowed root ids (spec §5.1 `roots`).
    #[serde(default)]
    pub roots: Vec<Uuid>,
    /// Explicitly allowed Qdrant collection names.
    #[serde(default)]
    pub collections: Vec<String>,
}

impl DomainScope {
    /// May this key act on the given root?
    #[must_use]
    pub fn allows_root(&self, root_id: &Uuid) -> bool {
        self.all || self.roots.contains(root_id)
    }
}

/// The authenticated principal for a request, derived entirely server-side.
///
/// `tenant_id` is the isolation boundary and is **never** taken from client
/// input (spec §12.7).
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// Tenant the principal belongs to (server-derived).
    pub tenant_id: Uuid,
    /// User id, if the principal is a user (vs. a pure service key).
    pub user_id: Option<Uuid>,
    /// API key id used, if any (for audit, spec §5.1 `audit_log.actor_key_id`).
    pub api_key_id: Option<Uuid>,
    /// Effective roles (union of user_roles).
    pub roles: Vec<Role>,
    /// Effective resource scopes.
    pub scopes: Vec<Scope>,
    /// Domain scope restricting collections/roots.
    pub domain_scope: DomainScope,
}

impl AuthContext {
    /// Highest role held, for quick min-role checks.
    #[must_use]
    pub fn max_role(&self) -> Option<Role> {
        self.roles.iter().copied().max()
    }

    /// Enforce a minimum role; returns [`AppError::Forbidden`] otherwise.
    pub fn require_role(&self, required: Role) -> Result<(), AppError> {
        match self.max_role() {
            Some(r) if r.satisfies(required) => Ok(()),
            _ => Err(AppError::Forbidden {
                message: format!("requires role {required:?}"),
            }),
        }
    }

    /// Enforce possession of a resource scope; returns [`AppError::Forbidden`] otherwise.
    pub fn require_scope(&self, required: &Scope) -> Result<(), AppError> {
        if self.scopes.contains(required) {
            Ok(())
        } else {
            Err(AppError::Forbidden {
                message: format!("missing scope {required:?}"),
            })
        }
    }
}

/// Hash a freshly generated raw API key with argon2 for storage (spec §12.2).
///
/// Returns the PHC-format hash string to persist in `api_keys.key_hash`. The raw
/// key is shown to the user exactly once and never stored.
pub fn hash_api_key(_raw_key: &str) -> Result<String, AppError> {
    // TODO: use argon2::Argon2::default() with a random salt
    // (argon2::password_hash::SaltString::generate(&mut OsRng)) and
    // PasswordHasher::hash_password; return the serialized PHC string.
    Err(AppError::Internal {
        message: "hash_api_key not yet implemented".to_owned(),
    })
}

/// Constant-time verify a presented raw key against a stored argon2 hash (spec §12.2).
pub fn verify_api_key(_raw_key: &str, _stored_hash: &str) -> Result<bool, AppError> {
    // TODO: parse stored_hash via argon2::PasswordHash::new and call
    // Argon2::default().verify_password; map a verification mismatch to Ok(false)
    // and any structural error to AppError::Internal.
    Err(AppError::Internal {
        message: "verify_api_key not yet implemented".to_owned(),
    })
}

/// Derive the short, non-secret prefix shown in the UI for a raw key
/// (spec §5.1 `api_keys.prefix`).
#[must_use]
pub fn key_prefix(raw_key: &str) -> String {
    raw_key.chars().take(8).collect()
}

/// Verified JWT claims (spec §12.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject (user id).
    pub sub: String,
    /// Issuer.
    pub iss: String,
    /// Audience.
    pub aud: String,
    /// Tenant slug/id encoded in the token.
    pub tenant: String,
    /// Expiry (unix seconds).
    pub exp: usize,
}

/// Verify and decode a bearer JWT (spec §12.2).
///
/// Validates signature, issuer, audience, and expiry against the configured
/// public key. Returns typed [`Claims`] on success.
pub fn verify_jwt(
    _token: &str,
    _public_key_pem: &[u8],
    _issuer: &str,
    _audience: &str,
) -> Result<Claims, AppError> {
    // TODO: build jsonwebtoken::Validation (set_issuer/set_audience), construct a
    // DecodingKey from the PEM, and jsonwebtoken::decode::<Claims>. Map any
    // validation failure to AppError::Unauthorized.
    Err(AppError::Unauthorized {
        message: "verify_jwt not yet implemented".to_owned(),
    })
}
