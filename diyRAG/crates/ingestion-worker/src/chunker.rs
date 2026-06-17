//! Structure-aware chunker (MASTER_BUILD_SPEC.md §6.4).
//!
//! Splits on paragraph/heading/table boundaries first, then packs to a target
//! token window using the `tokenizers` crate. Defaults: **~512 tokens, 64–96
//! overlap**, configurable per collection. Tables are kept intact. Chunks that
//! fail invariants (empty text, over the hard token cap, missing required
//! metadata) are routed to the quarantine path (§6.4).

use serde::{Deserialize, Serialize};

use crate::parser::{StructureType, StructuredDoc};
use crate::WorkUnit;

/// Per-collection chunking parameters (§6.4).
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    pub target_tokens: usize,
    pub overlap_tokens: usize,
    pub hard_max_tokens: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_tokens: 512,
            overlap_tokens: 80, // mid-point of the 64–96 band (§6.4)
            hard_max_tokens: 1024,
        }
    }
}

/// A chunk in transit, carrying the mandatory metadata of §5.4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub chunk_id: uuid::Uuid,
    pub tenant_id: uuid::Uuid,
    pub document_id: uuid::Uuid,
    pub ordinal: usize,
    pub text: String,
    pub token_count: usize,
    pub section_heading: Option<String>,
    pub page_number: Option<u32>,
    pub structure_type: StructureType,
}

/// Chunking failures. `Invariant` is a quarantine reason, not a retryable error
/// (§6.4 / §14).
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error("chunk invariant violated: {0}")]
    Invariant(String),
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
}

/// Structure-aware chunker holding the loaded tokenizer + config.
pub struct Chunker {
    config: ChunkConfig,
    // TODO: tokenizer: tokenizers::Tokenizer, loaded once from a pinned model.
}

impl Chunker {
    pub fn new(config: ChunkConfig) -> Self {
        Self { config }
    }

    /// Produce ordered chunks for one document (§6.4).
    pub fn chunk(&self, _doc: &StructuredDoc, _unit: &WorkUnit) -> Result<Vec<Chunk>, ChunkError> {
        // TODO:
        //   1. Walk blocks; start a new chunk at heading/table boundaries.
        //   2. Pack prose blocks until `config.target_tokens`, counting tokens
        //      via the loaded tokenizer.
        //   3. Carry `config.overlap_tokens` from the previous chunk.
        //   4. Keep Table blocks whole; tag structure_type accordingly.
        //   5. Stamp mandatory metadata (§5.4) and validate via `check_invariants`.
        let _ = &self.config;
        Ok(Vec::new())
    }

    /// Reject chunks failing the §6.4 invariants → caller sends them to quarantine.
    fn check_invariants(&self, chunk: &Chunk) -> Result<(), ChunkError> {
        if chunk.text.trim().is_empty() {
            return Err(ChunkError::Invariant("empty chunk text".into()));
        }
        if chunk.token_count > self.config.hard_max_tokens {
            return Err(ChunkError::Invariant(format!(
                "token_count {} exceeds hard max {}",
                chunk.token_count, self.config.hard_max_tokens
            )));
        }
        Ok(())
    }
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new(ChunkConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chunk(text: &str, tokens: usize) -> Chunk {
        Chunk {
            chunk_id: uuid::Uuid::now_v7(),
            tenant_id: uuid::Uuid::now_v7(),
            document_id: uuid::Uuid::now_v7(),
            ordinal: 0,
            text: text.to_string(),
            token_count: tokens,
            section_heading: None,
            page_number: None,
            structure_type: StructureType::Prose,
        }
    }

    #[test]
    fn empty_chunk_is_rejected() {
        let c = Chunker::default();
        assert!(c.check_invariants(&sample_chunk("   ", 1)).is_err());
    }

    #[test]
    fn oversized_chunk_is_rejected() {
        let c = Chunker::default();
        assert!(c.check_invariants(&sample_chunk("ok", 99_999)).is_err());
    }

    #[test]
    fn valid_chunk_passes() {
        let c = Chunker::default();
        assert!(c.check_invariants(&sample_chunk("hello", 2)).is_ok());
    }
}
