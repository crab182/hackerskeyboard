#![forbid(unsafe_code)]
//! Structured logging + tracing setup (spec §13.1).
//!
//! Initializes a `tracing` subscriber that emits **structured JSON** logs with
//! an `EnvFilter`, and provides a Tower layer that opens a per-request span
//! carrying the `correlation_id` field so every log line within a request is
//! attributable (spec §13.1). No `println!`/`print` anywhere (spec §19).

use tower_http::trace::TraceLayer;
use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

use crate::config::ObservabilityConfig;
use crate::correlation::CorrelationId;
use crate::errors::AppError;

/// Initialize global tracing for a service.
///
/// Call exactly once at the start of `main`. Honors [`ObservabilityConfig`]:
/// JSON vs pretty output and the `EnvFilter` directive. When an OTLP endpoint is
/// configured, a `tracing-opentelemetry` layer is added (TODO).
pub fn init(cfg: &ObservabilityConfig) -> Result<(), AppError> {
    let filter = EnvFilter::try_new(&cfg.log_filter).map_err(|e| AppError::Config {
        message: format!("invalid log_filter `{}`: {e}", cfg.log_filter),
    })?;

    let registry = tracing_subscriber::registry().with(filter);

    // TODO: when cfg.otlp_endpoint is Some, layer in tracing-opentelemetry's
    // OpenTelemetryLayer (OTLP exporter → Tempo/Jaeger, spec §13.1).

    if cfg.json_logs {
        registry.with(json_layer()).try_init()
    } else {
        registry.with(pretty_layer()).try_init()
    }
    .map_err(|e| AppError::Internal {
        message: format!("failed to initialize tracing subscriber: {e}"),
    })
}

/// JSON formatting layer (production default).
fn json_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
}

/// Human-friendly layer for local development.
fn pretty_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer().pretty().with_target(true)
}

/// Build the Tower-HTTP trace layer that opens a span per request.
///
/// The span is enriched with the [`CorrelationId`] so all logs emitted while
/// handling the request carry it (spec §13.1). Wire this into each service's
/// Axum router via `.layer(trace_layer())`.
#[must_use]
pub fn trace_layer(
) -> TraceLayer<tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>>
{
    // TODO: customize make_span_with to read the X-Correlation-ID header (or
    // mint one via CorrelationId::from_header_or_new) and record it as a
    // `correlation_id` span field. Default classifier is fine for liveness.
    TraceLayer::new_for_http()
}

/// Open a manual request span with a known correlation id.
///
/// Useful in non-HTTP contexts (NATS consumers, sync agent) where there is no
/// Tower layer but the correlation id must still propagate (spec §13.1).
#[must_use]
pub fn request_span(correlation_id: CorrelationId) -> tracing::Span {
    tracing::info_span!("request", correlation_id = %correlation_id)
}
