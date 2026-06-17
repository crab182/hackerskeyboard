//! Parser router and the pluggable [`Parser`] trait (MASTER_BUILD_SPEC.md §6.3).
//!
//! A [`ParserRouter`] selects a handler by **magic-byte MIME sniff** (`infer`),
//! never by extension, and falls back to extension only when sniffing is
//! inconclusive. New formats are added by implementing [`Parser`] and registering
//! the handler — no core changes (§6.3).
//!
//! Every handler is defensively coded: tokio timeouts, memory caps, and a
//! "never trust the file" posture (§12.4, §12.5, §22 row 6).

pub mod ebook;
pub mod epub;
pub mod markup;
pub mod ooxml;
pub mod pdf;
pub mod spreadsheet;
pub mod structured;

use async_trait::async_trait;
use bytes::Bytes;

/// Result of magic-byte sniffing plus the declared filename (for extension
/// fallback only). Tenant/trust decisions never depend on this — it only steers
/// handler selection (§6.3).
#[derive(Debug, Clone)]
pub struct MimeSniff {
    /// True MIME from `infer` magic bytes, if recognized.
    pub sniffed_mime: Option<String>,
    /// Lower-cased extension parsed from the declared name (fallback signal).
    pub extension: Option<String>,
}

/// How confident a handler is that it can parse a given input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Cannot handle this input.
    No,
    /// Could handle via the extension fallback path.
    Maybe,
    /// Magic bytes match this handler's format.
    Yes,
}

/// A reference to the original bytes plus identifying metadata (§5.3 / §6.3).
#[derive(Debug, Clone)]
pub struct BlobRef {
    /// Content-addressed blob key (`sha256/{first2}/{sha256}`).
    pub key: String,
    /// Original bytes fetched from the blob store.
    pub bytes: Bytes,
    /// Declared filename from the source; used only for extension fallback.
    pub declared_name: String,
}

/// Per-parse options (timeouts, OCR forcing, size caps) (§6.3).
#[derive(Debug, Clone)]
pub struct ParseOpts {
    /// Hard wall-clock cap on a single parse (defense vs. parser bombs, §22).
    pub timeout: std::time::Duration,
    /// Force the OCR/Python path even if text density looks adequate (§6.3).
    pub force_ocr: bool,
    /// Reject inputs larger than this many bytes after decompression (§12.4).
    pub max_bytes: usize,
}

impl Default for ParseOpts {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(120),
            force_ocr: false,
            max_bytes: 256 * 1024 * 1024,
        }
    }
}

/// A normalized parsed document: ordered text blocks with structure, headings,
/// page coordinates, and tables-as-markdown (§6.3).
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructuredDoc {
    pub blocks: Vec<Block>,
    pub lang: Option<String>,
    pub page_count: Option<u32>,
}

/// One structural unit of a document. `structure_type` mirrors §5.1.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub text: String,
    pub structure_type: StructureType,
    pub section_heading: Option<String>,
    pub page_number: Option<u32>,
}

/// `chunks.structure_type` domain (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StructureType {
    Prose,
    Table,
    Heading,
    Code,
    Triple,
}

/// Parser error taxonomy. `Permanent` goes straight to quarantine; `Transient`
/// is eligible for retry/backoff (§14).
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("no handler matched the input")]
    NoHandler,
    #[error("parse timed out")]
    Timeout,
    #[error("input rejected by safety bounds: {0}")]
    Rejected(String),
    #[error("permanent parse failure: {0}")]
    Permanent(String),
    #[error("transient parse failure: {0}")]
    Transient(String),
    #[error("delegated parsing-service error: {0}")]
    Delegated(String),
}

/// The pluggable parser contract (verbatim shape from §6.3).
#[async_trait]
pub trait Parser: Send + Sync {
    /// Cheap, side-effect-free capability probe used by the router.
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence;
    /// Parse the blob into a normalized [`StructuredDoc`].
    async fn parse(&self, blob: BlobRef, opts: &ParseOpts) -> Result<StructuredDoc, ParseError>;
}

/// Selects a [`Parser`] by MIME sniff with extension fallback (§6.3).
pub struct ParserRouter {
    handlers: Vec<Box<dyn Parser>>,
}

impl ParserRouter {
    /// Build a router with all required handlers registered (§6.3 table).
    pub fn with_defaults() -> Self {
        let handlers: Vec<Box<dyn Parser>> = vec![
            Box::new(pdf::PdfTextParser::new()),
            Box::new(ooxml::OoxmlParser::new()),
            Box::new(spreadsheet::SpreadsheetParser::new()),
            Box::new(markup::MarkupParser::new()),
            Box::new(epub::EpubParser::new()),
            Box::new(ebook::EbookConvertParser::new()),
            Box::new(structured::StructuredParser::new()),
        ];
        Self { handlers }
    }

    /// Register an additional handler (plugin extension point, §6.3).
    pub fn register(&mut self, handler: Box<dyn Parser>) {
        self.handlers.push(handler);
    }

    /// Sniff, select the highest-confidence handler, and parse under a timeout.
    pub async fn route_and_parse(
        &self,
        blob: &BlobRef,
        opts: &ParseOpts,
    ) -> Result<StructuredDoc, ParseError> {
        if blob.bytes.len() > opts.max_bytes {
            return Err(ParseError::Rejected("input exceeds max_bytes".into()));
        }
        let sniff = Self::sniff(blob);
        let handler = self.select(&sniff).ok_or(ParseError::NoHandler)?;

        // Enforce the per-parse wall-clock cap regardless of handler (§22 row 6).
        match tokio::time::timeout(opts.timeout, handler.parse(blob.clone(), opts)).await {
            Ok(res) => res,
            Err(_) => Err(ParseError::Timeout),
        }
    }

    /// Magic-byte sniff (`infer`) plus extension extraction (§6.3).
    pub fn sniff(blob: &BlobRef) -> MimeSniff {
        let sniffed_mime = infer::get(&blob.bytes).map(|t| t.mime_type().to_string());
        let extension = blob
            .declared_name
            .rsplit('.')
            .next()
            .filter(|e| !e.is_empty() && *e != blob.declared_name)
            .map(|e| e.to_ascii_lowercase());
        MimeSniff {
            sniffed_mime,
            extension,
        }
    }

    /// Pick the handler with the strongest [`Confidence`]; ties keep registration
    /// order (deterministic — §19).
    fn select(&self, sniff: &MimeSniff) -> Option<&dyn Parser> {
        self.handlers
            .iter()
            .map(|h| (h.can_handle(sniff), h))
            .filter(|(c, _)| *c != Confidence::No)
            .max_by_key(|(c, _)| *c)
            .map(|(_, h)| h.as_ref())
    }
}

impl Default for ParserRouter {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff_for(name: &str, ext: Option<&str>) -> MimeSniff {
        MimeSniff {
            sniffed_mime: name.is_empty().then(|| String::new()).filter(|_| false),
            extension: ext.map(|e| e.to_string()),
        }
    }

    #[test]
    fn extension_is_extracted_lowercased() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"hello"),
            declared_name: "Report.PDF".into(),
        };
        let s = ParserRouter::sniff(&blob);
        assert_eq!(s.extension.as_deref(), Some("pdf"));
    }

    #[test]
    fn router_selects_a_handler_by_extension_fallback() {
        let router = ParserRouter::with_defaults();
        // TODO: assert markup handler is chosen for a `.md` extension once the
        // markup parser reports Confidence::Maybe on it.
        let _ = router.select(&sniff_for("", Some("md")));
    }
}
