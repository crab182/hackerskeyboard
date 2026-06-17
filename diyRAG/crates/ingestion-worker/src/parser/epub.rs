//! EPUB parser via the `epub` crate (MASTER_BUILD_SPEC.md §6.3).
//!
//! Walks the spine in reading order, parses each XHTML document, and reuses the
//! markup sanitization pass (§12.5) since EPUB content is HTML.

use async_trait::async_trait;

use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

const EPUB_MIME: &str = "application/epub+zip";

/// Handles `.epub`.
pub struct EpubParser;

impl EpubParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EpubParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EpubParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        if sniff.sniffed_mime.as_deref() == Some(EPUB_MIME) {
            Confidence::Yes
        } else if sniff.extension.as_deref() == Some("epub") {
            Confidence::Maybe
        } else {
            Confidence::No
        }
    }

    async fn parse(&self, _blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        // TODO: epub::doc::EpubDoc::from_reader(Cursor::new(bytes)); iterate the
        //       spine in order, parse each XHTML resource, reuse
        //       MarkupParser::sanitize_text, emit Blocks with chapter headings.
        Err(ParseError::Permanent("epub parsing not yet implemented".into()))
    }
}
