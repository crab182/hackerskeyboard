#![forbid(unsafe_code)]
//! Gateway handler/extractor error newtype (spec §11.3).
//!
//! [`diyrag_common::errors::AppError`] is a foreign type, so the orphan rule
//! forbids `impl IntoResponse for AppError` in this crate. [`GatewayError`] wraps
//! it so both the route handlers (returning `Result<Response, GatewayError>`) and
//! the auth extractor (`FromRequestParts::Rejection = GatewayError`) can render
//! the standard error envelope. `?` converts any `AppError` into this wrapper via
//! the [`From`] impl.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use diyrag_common::correlation::CorrelationId;
use diyrag_common::errors::AppError;

/// Newtype over [`AppError`] that renders the spec §11.3 envelope.
pub struct GatewayError(pub AppError);

impl From<AppError> for GatewayError {
    fn from(e: AppError) -> Self {
        GatewayError(e)
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let err = self.0;
        let status =
            StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // TODO: write the error_log row (common/core-api) and use its log_id as
        //       the reference_code so the GUI deep-link resolves (spec §10.4/§13.2).
        let envelope = err.to_envelope("ERR-PENDING", CorrelationId::new());
        (status, Json(envelope)).into_response()
    }
}
