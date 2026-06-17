//! OOXML parser for `docx` / `pptx` via `zip` + `quick-xml` (MASTER_BUILD_SPEC.md §6.3).
//!
//! OOXML files are ZIP containers of XML parts. We stream the document/slide
//! parts with `quick-xml`, recover the heading hierarchy, and render tables to
//! markdown. Complex layout that defeats the structural reader is delegated to
//! Docling in the Python parsing-service (§6.3).

use async_trait::async_trait;

use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const PPTX_MIME: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// Handles `.docx` and `.pptx`.
pub struct OoxmlParser;

impl OoxmlParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OoxmlParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for OoxmlParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        match sniff.sniffed_mime.as_deref() {
            // `infer` reports OOXML containers as zip; magic alone can't tell
            // docx from xlsx, so rely on extension to disambiguate (§6.3).
            Some(DOCX_MIME) | Some(PPTX_MIME) => Confidence::Yes,
            _ => match sniff.extension.as_deref() {
                Some("docx") | Some("pptx") => Confidence::Maybe,
                _ => Confidence::No,
            },
        }
    }

    async fn parse(&self, _blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        // TODO: open the bytes as a zip archive; for docx read `word/document.xml`,
        //       for pptx iterate `ppt/slides/slideN.xml`; stream with quick-xml,
        //       map paragraph styles → headings, tables → markdown Blocks.
        //       On structural failure, delegate to Docling (parsing-service).
        Err(ParseError::Permanent("ooxml parsing not yet implemented".into()))
    }
}
