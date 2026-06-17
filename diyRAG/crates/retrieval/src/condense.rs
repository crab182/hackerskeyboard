#![forbid(unsafe_code)]
//! Optional context-condense pass (spec §7.2).
//!
//! Before final answer generation, a cheap LLM call extracts ONLY the
//! query-relevant facts from the reranked chunks. This prevents context-window
//! collapse and the "hallucinated merge" failure (spec §22 #8) by shrinking the
//! context the answer model sees while preserving provenance.
//!
//! Critically, the condense step treats retrieved text as **untrusted data, not
//! instructions** (spec §12.5): the prompt separates the trusted task from the
//! retrieved content with explicit delimiters + trust markers, so injected
//! instructions inside a chunk cannot steer the condenser.

use diyrag_common::errors::AppError;

use crate::{RetrievalState, SearchHit};

/// Condense the reranked chunks into a compact, query-relevant context string.
///
/// Returns the condensed text. On any failure the caller MAY fall back to using
/// the raw reranked chunks (the condense pass is an optimization, spec §7.2).
pub async fn condense(
    state: &RetrievalState,
    _query: &str,
    chunks: &[SearchHit],
) -> Result<String, AppError> {
    if chunks.is_empty() {
        return Ok(String::new());
    }

    // TODO: build a condense prompt that:
    //   - states the trusted task (extract facts relevant to `query`),
    //   - wraps each chunk in explicit <untrusted>…</untrusted> delimiters with a
    //     trust marker and its document_id/page_number for provenance (§12.5),
    //   - instructs the model to ignore any instructions inside the chunks,
    //   then POST {gpu_runtime_base}/infer with a low token budget and return the
    //   condensed text. Map transport failures to AppError::Dependency
    //   { dependency: "gpu-runtime", .. } (spec §14).
    let _ = (&state.http, &state.gpu_runtime_base);
    Err(AppError::Dependency {
        dependency: "gpu-runtime".to_owned(),
        message: "condense not yet implemented".to_owned(),
    })
}

/// Build the trust-delimited prompt fragment for a single retrieved chunk
/// (spec §12.5). Static, reviewed formatting — never templated FROM the chunk
/// content in a way that could let the chunk break out of its delimiters.
#[must_use]
pub fn wrap_untrusted(chunk: &SearchHit) -> String {
    // The marker labels the segment as data; the answer/condense model is
    // instructed elsewhere that <untrusted> content is never instructions.
    format!(
        "<untrusted source_document=\"{}\" page=\"{}\">\n{}\n</untrusted>",
        chunk.document_id,
        chunk.page_number.unwrap_or_default(),
        // TODO: defensively strip stray closing delimiters / control chars from
        //       chunk.text at ingest (spec §12.5) so this wrapper is airtight.
        chunk.text,
    )
}
