#![forbid(unsafe_code)]
//! Gateway authentication & authorization (spec §12.2, §12.6, §12.7).
//!
//! This module turns an inbound credential (API key in `Authorization: ApiKey …`
//! or a bearer JWT) into a server-derived [`AuthContext`] (from `diyrag-common`),
//! then enforces scope / role / domain-scope checks. **All access-control
//! decisions are deterministic code OUTSIDE the LLM** (spec §0/§12) and the
//! tenant id is ALWAYS derived from the authenticated principal, never from
//! client input (spec §12.7).
//!
//! The heavy lifting (argon2 verify, JWT verify, scope/role math) already lives
//! in [`diyrag_common::auth`]; this module is the Axum-facing adapter that:
//! - extracts and parses the credential off the request,
//! - looks up the key/user in Postgres (instant-revocation check, spec §12.2),
//! - assembles the [`AuthContext`], and
//! - exposes an Axum extractor + a middleware that requires a minimum
//!   role/scope before business logic runs (spec §12.6).

use std::sync::Arc;

use axum::extract::FromRequestParts;
use diyrag_common::auth::{AuthContext, DomainScope, Role, Scope};
use diyrag_common::correlation::CorrelationId;
use diyrag_common::errors::AppError;
use http::request::Parts;
use uuid::Uuid;

use crate::GatewayState;

/// The `Authorization` header scheme prefix for API keys.
pub const API_KEY_SCHEME: &str = "ApiKey";
/// The `Authorization` header scheme prefix for bearer JWTs.
pub const BEARER_SCHEME: &str = "Bearer";

/// A parsed, not-yet-verified credential lifted off the `Authorization` header.
#[derive(Debug, Clone)]
pub enum Credential {
    /// A raw API key (`Authorization: ApiKey <raw>`). Verified against the
    /// argon2 hash in `api_keys.key_hash` (spec §12.2).
    ApiKey(String),
    /// A bearer JWT (`Authorization: Bearer <jwt>`). Verified against the
    /// configured public key (spec §12.2).
    Jwt(String),
}

impl Credential {
    /// Parse the `Authorization` header value into a [`Credential`].
    ///
    /// Returns [`AppError::Unauthorized`] when the header is missing or uses an
    /// unsupported scheme. Does NOT verify the credential.
    pub fn parse(header: Option<&str>) -> Result<Self, AppError> {
        let value = header.ok_or_else(|| AppError::Unauthorized {
            message: "missing Authorization header".to_owned(),
        })?;
        let (scheme, token) = value
            .split_once(' ')
            .ok_or_else(|| AppError::Unauthorized {
                message: "malformed Authorization header".to_owned(),
            })?;
        let token = token.trim().to_owned();
        if scheme.eq_ignore_ascii_case(API_KEY_SCHEME) {
            Ok(Credential::ApiKey(token))
        } else if scheme.eq_ignore_ascii_case(BEARER_SCHEME) {
            Ok(Credential::Jwt(token))
        } else {
            Err(AppError::Unauthorized {
                message: format!("unsupported auth scheme `{scheme}`"),
            })
        }
    }
}

/// Authenticate a [`Credential`] against the backing store / JWT key and produce
/// a fully server-derived [`AuthContext`] (spec §12.7).
///
/// This is the single choke point through which every request's principal is
/// established. The `correlation_id` is threaded for audit/log linkage.
pub async fn authenticate(
    state: &GatewayState,
    credential: Credential,
    _correlation_id: CorrelationId,
) -> Result<AuthContext, AppError> {
    match credential {
        Credential::ApiKey(raw) => authenticate_api_key(state, &raw).await,
        Credential::Jwt(token) => authenticate_jwt(state, &token).await,
    }
}

/// Verify a raw API key: prefix-lookup the candidate row(s), argon2-verify, and
/// reject revoked/expired keys (instant revocation, spec §12.2).
async fn authenticate_api_key(state: &GatewayState, raw: &str) -> Result<AuthContext, AppError> {
    // Non-secret prefix narrows the candidate set without leaking the key.
    let _prefix = diyrag_common::auth::key_prefix(raw);

    // TODO: SELECT id, tenant_id, user_id, key_hash, scopes, domain_scope,
    //       expires_at, revoked_at FROM api_keys WHERE prefix = $1 (parameterized,
    //       spec §12.4) using state.db pool. For each candidate:
    //         - reject if revoked_at IS NOT NULL or expires_at < now() (§12.2),
    //         - diyrag_common::auth::verify_api_key(raw, &key_hash)? to match,
    //         - on match, UPDATE last_used_at = now() and load the user's roles
    //           via user_roles, then build the AuthContext below.
    //       Use a negative cache + DB check for instant revocation (§12.2).
    let _ = state;
    Err(AppError::Unauthorized {
        message: "api-key authentication not yet implemented".to_owned(),
    })
}

/// Verify a bearer JWT and map its claims to an [`AuthContext`].
async fn authenticate_jwt(state: &GatewayState, token: &str) -> Result<AuthContext, AppError> {
    let auth_cfg = &state.config.auth;

    // TODO: load the verification public key from auth_cfg.jwt_public_key_path
    //       (cache it in GatewayState), then
    //       diyrag_common::auth::verify_jwt(token, &pem, &auth_cfg.jwt_issuer,
    //       &auth_cfg.jwt_audience)?. Resolve claims.tenant -> tenant_id and
    //       claims.sub -> user_id against Postgres, load roles/scopes, and build
    //       the AuthContext. Tenant id is taken from the verified token, never a
    //       client-supplied parameter (spec §12.7).
    let _ = (token, auth_cfg);
    Err(AppError::Unauthorized {
        message: "jwt authentication not yet implemented".to_owned(),
    })
}

/// Axum extractor that authenticates the request and yields the [`AuthContext`].
///
/// Wraps [`AuthContext`] so we can implement the foreign-trait extractor here
/// (orphan rule) while reusing the common type unchanged.
#[derive(Debug, Clone)]
pub struct Authenticated(pub AuthContext);

impl FromRequestParts<GatewayState> for Authenticated {
    // The rejection must be `IntoResponse`; `AppError` is a foreign type and is
    // not, so we reject with the crate's `GatewayError` wrapper. `?` on the
    // `AppError`-returning calls below converts via `From<AppError>`.
    type Rejection = crate::error::GatewayError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &GatewayState,
    ) -> Result<Self, Self::Rejection> {
        let correlation_id = CorrelationId::from_header_or_new(
            parts
                .headers
                .get(diyrag_common::correlation::HEADER_NAME)
                .and_then(|v| v.to_str().ok()),
        );
        let header = parts
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let credential = Credential::parse(header)?;
        let ctx = authenticate(state, credential, correlation_id).await?;
        Ok(Authenticated(ctx))
    }
}

/// A declarative requirement attached to a route, checked before business logic
/// runs (spec §12.6 — "permission checks in Tower middleware before business
/// logic"). Combines a minimum role, a required scope, and optional domain
/// scoping against a target root.
#[derive(Debug, Clone)]
pub struct Requirement {
    /// Minimum RBAC role (Reader < Ingester < Admin).
    pub min_role: Role,
    /// Resource scope the key/principal must hold.
    pub scope: Scope,
}

impl Requirement {
    /// Reader-level read/query requirement.
    #[must_use]
    pub fn reader() -> Self {
        Self {
            min_role: Role::Reader,
            scope: Scope::Reader,
        }
    }

    /// Ingest-level requirement (register roots, add files; spec §11.2).
    #[must_use]
    pub fn ingest() -> Self {
        Self {
            min_role: Role::Ingester,
            scope: Scope::Ingest,
        }
    }

    /// Admin-level requirement (keys, nodes, purge; spec §11.2).
    #[must_use]
    pub fn admin() -> Self {
        Self {
            min_role: Role::Admin,
            scope: Scope::Admin,
        }
    }

    /// Enforce this requirement against an authenticated principal.
    ///
    /// Order: role first, then scope (both deny-by-default, spec §12.5).
    pub fn enforce(&self, ctx: &AuthContext) -> Result<(), AppError> {
        ctx.require_role(self.min_role)?;
        ctx.require_scope(&self.scope)?;
        Ok(())
    }
}

/// Enforce that a principal may act on a specific root (domain-scope check,
/// spec §5.1 `domain_scope` / §12.7). Tenant ownership of the root MUST be
/// re-verified server-side at the data layer; this is the gateway-side gate.
// Called from `routes::proxy_passthrough` once root-id extraction lands (its
// TODO); kept ahead of the caller so the gate exists and is reviewable.
#[allow(dead_code)]
pub fn check_domain_scope(domain: &DomainScope, root_id: &Uuid) -> Result<(), AppError> {
    if domain.allows_root(root_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden {
            message: format!("api key domain scope does not include root {root_id}"),
        })
    }
}

/// Shared, cheaply-cloneable handle to verification material the gateway caches
/// (e.g. the JWT public key, a negative-revocation cache). Held inside
/// [`GatewayState`] in a follow-up.
#[allow(dead_code)] // wired into GatewayState in a follow-up (spec §12.2)
#[derive(Clone, Default)]
pub struct AuthCache {
    /// PEM-encoded JWT verification key, loaded once at startup.
    pub jwt_public_key_pem: Arc<Vec<u8>>,
    // TODO: add a moka/`fred`-backed negative cache of revoked key ids for
    //       instant revocation without a DB hit on every request (§12.2).
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_key_and_bearer_schemes_case_insensitively() {
        match Credential::parse(Some("ApiKey raw-secret")) {
            Ok(Credential::ApiKey(k)) => assert_eq!(k, "raw-secret"),
            other => panic!("expected ApiKey, got {other:?}"),
        }
        match Credential::parse(Some("bearer jwt.token.here")) {
            Ok(Credential::Jwt(t)) => assert_eq!(t, "jwt.token.here"),
            other => panic!("expected Jwt, got {other:?}"),
        }
    }

    #[test]
    fn missing_malformed_and_unsupported_headers_are_unauthorized() {
        // Missing header, no scheme separator, and an unknown scheme all reject
        // with Unauthorized (deny-by-default, spec §12.5) — never a panic.
        for header in [None, Some("noscheme"), Some("Basic dXNlcjpwYXNz")] {
            assert!(
                matches!(
                    Credential::parse(header),
                    Err(AppError::Unauthorized { .. })
                ),
                "expected Unauthorized for {header:?}"
            );
        }
    }
}
