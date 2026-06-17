#![forbid(unsafe_code)]
//! Correlation IDs (spec §13.1).
//!
//! A `correlation_id` is generated at the api-gateway, injected into the
//! `X-Correlation-ID` header on every outbound HTTP request and into NATS
//! message headers, and carried as a `tracing` span field through every hop so
//! a full request can be reconstructed from logs/traces.
//!
//! This module provides:
//! - [`CorrelationId`] — a UUID newtype that (de)serializes transparently.
//! - [`HEADER_NAME`]   — the canonical header name.
//! - an Axum [`FromRequestParts`] extractor that reads an inbound correlation id
//!   or mints a fresh one.

use std::fmt;

use axum::extract::FromRequestParts;
use http::request::Parts;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Canonical header name carrying the correlation id between services.
pub const HEADER_NAME: &str = "x-correlation-id";

/// A request-scoped correlation identifier.
///
/// Wraps a [`Uuid`]; serializes as the bare UUID string so it slots directly
/// into the error envelope (spec §11.3) and log fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorrelationId(pub Uuid);

impl CorrelationId {
    /// Mint a fresh correlation id (UUIDv7 so it is time-sortable in logs).
    #[must_use]
    pub fn new() -> Self {
        Self(crate::ids::new_id())
    }

    /// Borrow the inner UUID.
    #[must_use]
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Parse a correlation id from an inbound header value, falling back to a
    /// freshly minted id when the value is absent or malformed.
    #[must_use]
    pub fn from_header_or_new(raw: Option<&str>) -> Self {
        match raw.and_then(|s| Uuid::parse_str(s.trim()).ok()) {
            Some(uuid) => Self(uuid),
            None => Self::new(),
        }
    }
}

impl Default for CorrelationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CorrelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Axum extractor: pulls the [`HEADER_NAME`] header off the request, or mints a
/// new id if missing/invalid. Infallible — every request always has an id.
impl<S> FromRequestParts<S> for CorrelationId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts.headers.get(HEADER_NAME).and_then(|v| v.to_str().ok());
        Ok(Self::from_header_or_new(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_header_and_falls_back_otherwise() {
        let id = Uuid::now_v7();
        assert_eq!(
            CorrelationId::from_header_or_new(Some(&id.to_string())).0,
            id
        );

        // Malformed or absent → a fresh v7 id (never a fixed/zero id).
        let from_garbage = CorrelationId::from_header_or_new(Some("not-a-uuid"));
        let from_none = CorrelationId::from_header_or_new(None);
        assert_eq!(from_garbage.as_uuid().get_version_num(), 7);
        assert_ne!(from_garbage.0, from_none.0, "two fresh mints must differ");
    }

    #[test]
    fn header_value_is_trimmed() {
        let id = Uuid::now_v7();
        let padded = format!("  {id}\t");
        assert_eq!(CorrelationId::from_header_or_new(Some(&padded)).0, id);
    }

    #[test]
    fn display_and_serde_are_transparent_strings() {
        let id = Uuid::now_v7();
        let c = CorrelationId(id);
        assert_eq!(c.to_string(), id.to_string());

        let json = serde_json::to_string(&c).expect("serialize");
        assert_eq!(json, format!("\"{id}\""));
        let back: CorrelationId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, c);
    }
}
