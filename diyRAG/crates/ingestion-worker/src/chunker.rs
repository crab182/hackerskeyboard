//! Structure-aware chunker (MASTER_BUILD_SPEC.md §6.4).
//!
//! Splits on heading/table/code boundaries first, then packs prose to a target
//! token window (counted with a deterministic whitespace-word estimate — see
//! [`Chunker`]). Defaults: **~512 tokens, 64–96 overlap**, configurable per
//! collection. Tables and code are kept intact. Chunks that fail invariants
//! (empty text, over the hard token cap) are routed to the quarantine path (§6.4).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// Structure-aware chunker.
///
/// Token counts use a deterministic **whitespace-word estimate** ([`estimate_tokens`])
/// rather than a loaded subword tokenizer: it needs no model (so chunking works
/// offline and is unit-testable), and subword tokenizers run ~1.2–1.4× higher, a
/// margin the `hard_max_tokens` cap absorbs. A real `tokenizers::Tokenizer` can be
/// swapped in behind this same boundary later without changing the algorithm.
pub struct Chunker {
    config: ChunkConfig,
}

impl Chunker {
    pub fn new(config: ChunkConfig) -> Self {
        Self { config }
    }

    /// Produce ordered chunks for one document (§6.4).
    ///
    /// Boundaries: a **Heading** updates the section context (and ends the current
    /// chunk); **Table**/**Triple**/**Code** blocks are emitted **whole** as their
    /// own chunk (never split or merged); **Prose** blocks are packed up to
    /// `target_tokens`, carrying an `overlap_tokens` tail into the next chunk for
    /// context continuity. Every chunk is validated by [`check_invariants`]; an
    /// over-cap block surfaces `ChunkError::Invariant` so the caller quarantines it.
    pub fn chunk(&self, doc: &StructuredDoc, unit: &WorkUnit) -> Result<Vec<Chunk>, ChunkError> {
        let tenant_id = unit.tenant_id;
        // INTERIM: document identity is content-addressed (deterministic on the
        // content hash) so re-ingest is idempotent (§6.2). TODO: have core-api
        // stamp the authoritative document_id into the WorkUnit at register time
        // and use that instead.
        let document_id = document_id_from_sha(&unit.content_sha256);
        let ids = (tenant_id, document_id);

        let mut chunks: Vec<Chunk> = Vec::new();
        let mut ordinal = 0usize;
        let mut heading: Option<String> = None;

        // Prose packing buffer.
        let mut buf = String::new();
        let mut buf_tokens = 0usize;
        let mut buf_heading: Option<String> = None;
        let mut buf_page: Option<u32> = None;

        for blk in &doc.blocks {
            match blk.structure_type {
                StructureType::Heading => {
                    if !buf.trim().is_empty() {
                        chunks.push(self.build_chunk(
                            ids,
                            ordinal,
                            &buf,
                            buf_tokens,
                            StructureType::Prose,
                            buf_heading.clone(),
                            buf_page,
                        )?);
                        ordinal += 1;
                    }
                    buf.clear();
                    buf_tokens = 0;
                    buf_heading = None;
                    buf_page = None;
                    heading = Some(blk.text.clone());
                }
                StructureType::Table | StructureType::Triple | StructureType::Code => {
                    // Kept whole: flush pending prose, then emit this block as its
                    // own chunk, preserving its structure_type.
                    if !buf.trim().is_empty() {
                        chunks.push(self.build_chunk(
                            ids,
                            ordinal,
                            &buf,
                            buf_tokens,
                            StructureType::Prose,
                            buf_heading.clone(),
                            buf_page,
                        )?);
                        ordinal += 1;
                        buf.clear();
                        buf_tokens = 0;
                        buf_heading = None;
                        buf_page = None;
                    }
                    let section = blk.section_heading.clone().or_else(|| heading.clone());
                    chunks.push(self.build_chunk(
                        ids,
                        ordinal,
                        &blk.text,
                        estimate_tokens(&blk.text),
                        blk.structure_type,
                        section,
                        blk.page_number,
                    )?);
                    ordinal += 1;
                }
                StructureType::Prose => {
                    let block_tokens = estimate_tokens(&blk.text);
                    // Flush before overflowing the target, seeding the next buffer
                    // with an overlap tail of the just-flushed text.
                    if buf_tokens > 0 && buf_tokens + block_tokens > self.config.target_tokens {
                        let overlap = tail_words(&buf, self.config.overlap_tokens);
                        chunks.push(self.build_chunk(
                            ids,
                            ordinal,
                            &buf,
                            buf_tokens,
                            StructureType::Prose,
                            buf_heading.clone(),
                            buf_page,
                        )?);
                        ordinal += 1;
                        buf = overlap;
                        buf_tokens = estimate_tokens(&buf);
                    }
                    if buf.trim().is_empty() {
                        buf_heading = blk.section_heading.clone().or_else(|| heading.clone());
                        buf_page = blk.page_number;
                    }
                    if !buf.is_empty() {
                        buf.push_str("\n\n");
                    }
                    buf.push_str(&blk.text);
                    buf_tokens = estimate_tokens(&buf);
                }
            }
        }
        if !buf.trim().is_empty() {
            chunks.push(self.build_chunk(
                ids,
                ordinal,
                &buf,
                buf_tokens,
                StructureType::Prose,
                buf_heading.clone(),
                buf_page,
            )?);
        }
        Ok(chunks)
    }

    /// Assemble one validated [`Chunk`], stamping the §5.4 mandatory metadata.
    fn build_chunk(
        &self,
        ids: (Uuid, Uuid),
        ordinal: usize,
        text: &str,
        token_count: usize,
        structure_type: StructureType,
        section_heading: Option<String>,
        page_number: Option<u32>,
    ) -> Result<Chunk, ChunkError> {
        let chunk = Chunk {
            chunk_id: Uuid::now_v7(),
            tenant_id: ids.0,
            document_id: ids.1,
            ordinal,
            text: text.trim().to_owned(),
            token_count,
            section_heading,
            page_number,
            structure_type,
        };
        self.check_invariants(&chunk)?;
        Ok(chunk)
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

/// Whitespace-word token estimate (see [`Chunker`] for the rationale).
fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The last `n` whitespace-delimited words of `text` (the overlap carried into
/// the next chunk). Empty when `n == 0`.
fn tail_words(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let start = words.len().saturating_sub(n);
    words[start..].join(" ")
}

/// Derive a deterministic document id from the content hash (first 16 bytes of
/// the sha256 hex). Same content → same id, giving idempotent re-ingest (§6.2).
fn document_id_from_sha(sha_hex: &str) -> Uuid {
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        if let Some(h) = sha_hex.get(i * 2..i * 2 + 2) {
            *b = u8::from_str_radix(h, 16).unwrap_or(0);
        }
    }
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Block;

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

    /// A chunker with a tiny window so packing/overlap/cap are exercised by short
    /// text: target 5 words, overlap 2, hard-max 8.
    fn small_chunker() -> Chunker {
        Chunker::new(ChunkConfig {
            target_tokens: 5,
            overlap_tokens: 2,
            hard_max_tokens: 8,
        })
    }

    fn blk(text: &str, kind: StructureType, heading: Option<&str>) -> Block {
        Block {
            text: text.to_string(),
            structure_type: kind,
            section_heading: heading.map(str::to_string),
            page_number: None,
        }
    }

    fn doc(blocks: Vec<Block>) -> StructuredDoc {
        StructuredDoc {
            blocks,
            lang: None,
            page_count: None,
        }
    }

    fn unit(sha: &str) -> WorkUnit {
        WorkUnit {
            work_unit_id: uuid::Uuid::now_v7(),
            job_id: uuid::Uuid::now_v7(),
            tenant_id: uuid::Uuid::now_v7(),
            document_ref: "doc.md".into(),
            content_sha256: sha.into(),
            blob_key: "k".into(),
            correlation_id: None,
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

    #[test]
    fn empty_document_yields_no_chunks() {
        let chunks = small_chunker().chunk(&doc(vec![]), &unit("ab")).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn packs_and_splits_with_overlap_and_sequential_ordinals() {
        // Three 3-word prose blocks, target 5 → splits into 3 chunks; each split
        // seeds the next with a 2-word overlap tail.
        let d = doc(vec![
            blk("a b c", StructureType::Prose, None),
            blk("d e f", StructureType::Prose, None),
            blk("g h i", StructureType::Prose, None),
        ]);
        let chunks = small_chunker().chunk(&d, &unit("ab")).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(
            chunks.iter().map(|c| c.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(chunks[0].text, "a b c");
        // Overlap: chunk 1 begins with the last 2 words of chunk 0.
        assert!(
            chunks[1].text.starts_with("b c"),
            "got {:?}",
            chunks[1].text
        );
        assert!(chunks.iter().all(|c| c.token_count <= 8));
        // Same document → one stable document_id across all chunks.
        assert!(chunks
            .iter()
            .all(|c| c.document_id == chunks[0].document_id));
    }

    #[test]
    fn heading_becomes_section_context_for_following_prose() {
        let d = doc(vec![
            blk("Introduction", StructureType::Heading, None),
            blk("hello world", StructureType::Prose, None),
        ]);
        let chunks = small_chunker().chunk(&d, &unit("ab")).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].structure_type, StructureType::Prose);
        assert_eq!(chunks[0].section_heading.as_deref(), Some("Introduction"));
        assert_eq!(chunks[0].text, "hello world");
    }

    #[test]
    fn table_and_code_blocks_are_kept_whole() {
        // Both kept-whole blocks stay within hard_max (8) here; an over-cap block
        // is quarantined instead — see `oversized_kept_whole_block_is_quarantined`.
        let d = doc(vec![
            blk("intro prose", StructureType::Prose, None),
            blk("a | b", StructureType::Table, None),
            blk("fn main", StructureType::Code, None),
        ]);
        let chunks = small_chunker().chunk(&d, &unit("ab")).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].structure_type, StructureType::Prose);
        assert_eq!(chunks[1].structure_type, StructureType::Table);
        assert!(chunks[1].text.contains("a | b"));
        assert_eq!(chunks[2].structure_type, StructureType::Code);
        assert!(chunks[2].text.contains("fn main"));
    }

    #[test]
    fn oversized_kept_whole_block_is_quarantined() {
        // A table over hard_max is not silently split; it surfaces Invariant so the
        // caller quarantines it (row-wise splitting of huge tables is a follow-up).
        let big_table = "| a | b | c | d | e | f | g | h | i | j |";
        let d = doc(vec![blk(big_table, StructureType::Table, None)]);
        let err = small_chunker().chunk(&d, &unit("ab")).unwrap_err();
        assert!(matches!(err, ChunkError::Invariant(_)), "got {err:?}");
    }

    #[test]
    fn block_over_hard_max_is_quarantined() {
        // 10 words > hard_max 8 → Invariant (the caller routes this to quarantine).
        let big = "one two three four five six seven eight nine ten";
        let d = doc(vec![blk(big, StructureType::Prose, None)]);
        let err = small_chunker().chunk(&d, &unit("ab")).unwrap_err();
        assert!(matches!(err, ChunkError::Invariant(_)), "got {err:?}");
    }

    #[test]
    fn document_id_is_deterministic_on_content_hash() {
        assert_eq!(
            document_id_from_sha("deadbeef"),
            document_id_from_sha("deadbeef")
        );
        assert_ne!(
            document_id_from_sha("deadbeef"),
            document_id_from_sha("feedface")
        );
    }
}
