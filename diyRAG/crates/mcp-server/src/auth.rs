#![forbid(unsafe_code)]
//! MCP authentication & server-derived authorization (MASTER_BUILD_SPEC.md §8, §12.7, §22 #5).
//!
//! The MCP server is a **thin protocol adapter** over `core-api`: it enforces the
//! **same** tenant scoping + RBAC as the REST API and is never a privilege bypass
//! (§8). Callers authenticate with **OAuth 2.1** (remote clients) or **mTLS**
//! (service callers). The **tenant scope is derived SERVER-side** from the
//! authenticated principal and is **never** taken from a client-supplied tool
//! argument (§12.7) — this is the single most important invariant in this file.

use diyrag_common::auth::{AuthContext, Role, Scope};
use diyrag_common::errors::AppError;

/// How an MCP caller authenticated (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// OAuth 2.1 bearer token (remote Streamable HTTP clients).
    OAuth21,
    /// Mutual TLS client identity (service-to-service callers).
    Mtls,
}

/// Authenticate an inbound MCP request and build a server-derived [`AuthContext`].
///
/// For Streamable HTTP this reads the `Authorization: Bearer …` OAuth 2.1 token;
/// for mTLS it reads the verified client-cert identity. The returned context's
/// `tenant_id`, roles, and scopes come **only** from the verified principal —
/// any tenant/collection hint in the request body is ignored (§12.7 / §22 #5).
pub fn authenticate(method: AuthMethod, credential: &str) -> Result<AuthContext, AppError> {
    let _ = (method, credential);
    // TODO:
    //  * OAuth21 → verify the JWT via diyrag_common::auth::verify_jwt against the
    //    configured issuer/audience/public key; map `sub`/`tenant`/roles/scopes
    //    from claims (looked up in Postgres for current revocation state, §12.2).
    //  * Mtls    → map the pinned client-cert identity to a service principal.
    //  Map any failure to AppError::Unauthorized. The tenant_id MUST come from
    //  the verified principal, never from tool arguments (§12.7).
    Err(AppError::Unauthorized {
        message: "MCP authenticate not yet implemented".to_owned(),
    })
}

/// Enforce the minimum role required for a tool (mirrors the REST RBAC, §12.6).
/// High-risk tools (ingest/admin) gate here BEFORE any adapter call to core-api.
pub fn require_role(ctx: &AuthContext, required: Role) -> Result<(), AppError> {
    ctx.require_role(required)
}

/// Enforce possession of a resource scope for a gated tool (§8 / §12.6).
pub fn require_scope(ctx: &AuthContext, required: &Scope) -> Result<(), AppError> {
    ctx.require_scope(required)
}

/// The server-derived tenant for this principal. Tools MUST call this rather than
/// reading any tenant field from their arguments (§12.7). Returns the tenant slug
/// usable to address the per-tenant Qdrant collection downstream.
#[must_use]
pub fn tenant_of(ctx: &AuthContext) -> uuid::Uuid {
    ctx.tenant_id
}
