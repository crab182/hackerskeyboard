//! PDF text parser with a scanned-detection heuristic (MASTER_BUILD_SPEC.md §6.3).
//!
//! The common, well-formed case is handled in pure Rust with `pdf-extract` /
//! `lopdf`. The "is this scanned?" decision is a cheap text-density heuristic;
//! only **hard** documents cross the Python boundary to the `parsing-service`
//! over gRPC (Surya/Marker OCR), keeping the common path Python-free (§3.3, §6.3).

use async_trait::async_trait;

use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

/// Rust-native PDF text handler with OCR delegation for scanned/complex docs.
pub struct PdfTextParser {
    /// Pages with fewer than this many extractable characters count as "image-like".
    min_chars_per_page: usize,
    /// If the fraction of text-bearing pages drops below this, delegate to OCR.
    min_text_page_ratio: f32,
}

impl PdfTextParser {
    pub fn new() -> Self {
        // DECISION: thresholds are defensible defaults; make them config-driven
        // per collection later (§6.4 says chunking is configurable; OCR triggers
        // belong with it).
        Self {
            min_chars_per_page: 32,
            min_text_page_ratio: 0.5,
        }
    }

    /// Cheap heuristic: extract per-page text and estimate density. Returns
    /// `true` when the document looks scanned/complex and should be OCR'd (§6.3).
    fn is_scanned(&self, _bytes: &[u8]) -> bool {
        // TODO: load with lopdf, iterate pages, count extracted chars per page,
        //       compute the ratio of pages above `min_chars_per_page`, and
        //       compare to `min_text_page_ratio`. Avoid full rasterization here —
        //       this must stay cheap (§6.3).
        false
    }

    /// Delegate a hard PDF to the Python parsing-service over gRPC (§3.3).
    async fn delegate_to_parsing_service(
        &self,
        _blob: &BlobRef,
        _opts: &ParseOpts,
    ) -> Result<StructuredDoc, ParseError> {
        // TODO: open a tonic mTLS channel to parsing-service, call OcrParse with
        //       the blob key (service fetches bytes from the shared blob store),
        //       map the response (text blocks + layout + tables) to StructuredDoc.
        Err(ParseError::Delegated(
            "parsing-service OCR delegation not yet implemented".into(),
        ))
    }
}

impl Default for PdfTextParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PdfTextParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        if sniff.sniffed_mime.as_deref() == Some("application/pdf") {
            Confidence::Yes
        } else if sniff.extension.as_deref() == Some("pdf") {
            Confidence::Maybe
        } else {
            Confidence::No
        }
    }

    async fn parse(&self, blob: BlobRef, opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        if opts.force_ocr || self.is_scanned(&blob.bytes) {
            return self.delegate_to_parsing_service(&blob, opts).await;
        }
        // TODO: pdf_extract::extract_text_from_mem(&blob.bytes) for fast text;
        //       use lopdf to recover page boundaries + headings → Block list.
        Err(ParseError::Permanent(
            "pdf text extraction not yet implemented".into(),
        ))
    }
}
