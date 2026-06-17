#![forbid(unsafe_code)]
//! MCP resources (MASTER_BUILD_SPEC.md §8).
//!
//! Read-only resources addressable by URI:
//!   * `document://{id}`   — a document's metadata + provenance,
//!   * `chunk://{id}`      — a single chunk's text + structure,
//!   * `collection://{tenant}` — a tenant collection summary.
//!
//! Each list/read result carries `ttlMs` + `cacheScope` so clients can cache
//! safely and the design stays forward-compatible (§8). Resources are subject to
//! the **same tenant scoping** as the tools: the tenant is server-derived and a
//! caller can only read resources within its own tenant (§12.7) — a crafted
//! `collection://other-tenant` URI is rejected.

use serde::{Deserialize, Serialize};

/// URI scheme prefixes for the three resource kinds (§8).
pub const SCHEME_DOCUMENT: &str = "document://";
pub const SCHEME_CHUNK: &str = "chunk://";
pub const SCHEME_COLLECTION: &str = "collection://";

/// Cache scope hint returned with every resource (§8 forward-compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheScope {
    /// Safe to cache per-tenant (shared across that tenant's sessions).
    Tenant,
    /// Cache only within the current session.
    Session,
    /// Never cache (e.g., volatile job status).
    None,
}

/// Caching metadata attached to a resource result (§8: `ttlMs` / `cacheScope`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheHint {
    /// Time-to-live in milliseconds.
    pub ttl_ms: u64,
    /// Scope within which the cached value is valid.
    pub cache_scope: CacheScope,
}

impl CacheHint {
    /// Default hint for stable, tenant-shareable metadata (documents/chunks).
    #[must_use]
    pub fn tenant_stable() -> Self {
        Self { ttl_ms: 60_000, cache_scope: CacheScope::Tenant }
    }
}

/// A parsed, validated resource reference (§8). Construction enforces the scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRef {
    Document(String),
    Chunk(String),
    Collection(String),
}

impl ResourceRef {
    /// Parse a resource URI into a typed reference. Unknown schemes are rejected
    /// so a caller cannot smuggle an arbitrary URI (§12.4). Note: tenant-scope
    /// enforcement happens in [`super::auth`] against the authenticated principal
    /// (§12.7) — parsing alone never authorizes access.
    #[must_use]
    pub fn parse(uri: &str) -> Option<Self> {
        if let Some(id) = uri.strip_prefix(SCHEME_DOCUMENT) {
            (!id.is_empty()).then(|| Self::Document(id.to_owned()))
        } else if let Some(id) = uri.strip_prefix(SCHEME_CHUNK) {
            (!id.is_empty()).then(|| Self::Chunk(id.to_owned()))
        } else if let Some(tenant) = uri.strip_prefix(SCHEME_COLLECTION) {
            (!tenant.is_empty()).then(|| Self::Collection(tenant.to_owned()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_schemes() {
        assert_eq!(
            ResourceRef::parse("document://abc"),
            Some(ResourceRef::Document("abc".to_owned()))
        );
        assert_eq!(
            ResourceRef::parse("chunk://c1"),
            Some(ResourceRef::Chunk("c1".to_owned()))
        );
        assert_eq!(
            ResourceRef::parse("collection://acme"),
            Some(ResourceRef::Collection("acme".to_owned()))
        );
    }

    #[test]
    fn rejects_unknown_or_empty() {
        assert_eq!(ResourceRef::parse("file:///etc/passwd"), None);
        assert_eq!(ResourceRef::parse("document://"), None);
        assert_eq!(ResourceRef::parse("nonsense"), None);
    }
}
