//! Markup parser for `html` / `md` / `txt` / `rtf` (MASTER_BUILD_SPEC.md §6.3),
//! with mandatory **hidden-text sanitization** per §12.5 / §22 row 1.
//!
//! Every ingested document is untrusted (§12.5). Before emitting blocks we strip
//! known hidden-instruction vectors: zero-width characters, control characters,
//! HTML comments, and off-screen / `font-size:0` / white-on-white text. This is
//! deterministic code outside the LLM (§12).

use async_trait::async_trait;
use unicode_normalization::UnicodeNormalization;

use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

/// Handles `text/html`, `text/markdown`, `text/plain`, and RTF.
pub struct MarkupParser;

impl MarkupParser {
    pub fn new() -> Self {
        Self
    }

    /// Strip hidden-instruction vectors and normalize unicode (§12.5).
    ///
    /// Applied to **all** extracted text before it becomes a [`super::Block`], so
    /// hidden prompt-injection payloads never reach the chunker or the model.
    pub fn sanitize_text(input: &str) -> String {
        // 1. Drop zero-width and control characters (keep \n and \t).
        let stripped: String = input
            .chars()
            .filter(|c| {
                !matches!(
                    *c,
                    // zero-width space/joiner/non-joiner, BOM, LTR/RTL marks.
                    '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{200E}' | '\u{200F}'
                ) && (!c.is_control() || *c == '\n' || *c == '\t')
            })
            .collect();
        // 2. NFC-normalize so homoglyph/decomposition tricks collapse (§12.5).
        let normalized: String = stripped.nfc().collect();
        // TODO: for HTML specifically, also drop comments and elements styled
        //       display:none / visibility:hidden / font-size:0 / color==background
        //       (handled in the html branch of `parse` before this call).
        normalized
    }
}

impl Default for MarkupParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for MarkupParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        match sniff.sniffed_mime.as_deref() {
            Some("text/html") | Some("text/plain") | Some("application/rtf")
            | Some("text/rtf") => Confidence::Yes,
            _ => match sniff.extension.as_deref() {
                Some("html") | Some("htm") | Some("md") | Some("markdown") | Some("txt")
                | Some("rtf") => Confidence::Maybe,
                _ => Confidence::No,
            },
        }
    }

    async fn parse(&self, _blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        // TODO: branch by detected/declared type:
        //   html → scraper DOM, drop hidden/off-screen nodes + comments, readability.
        //   md   → pulldown_cmark events → heading/prose/code/table Blocks.
        //   txt  → paragraph split.
        //   rtf  → strip control words to plaintext.
        // Then run `Self::sanitize_text` over every block's text (§12.5).
        Err(ParseError::Permanent("markup parsing not yet implemented".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_zero_width_and_controls() {
        let dirty = "Hello\u{200B}\u{0007} world\u{FEFF}!";
        let clean = MarkupParser::sanitize_text(dirty);
        assert_eq!(clean, "Hello world!");
    }

    #[test]
    fn sanitize_preserves_newlines_and_tabs() {
        let s = "line1\n\tline2";
        assert_eq!(MarkupParser::sanitize_text(s), s);
    }
}
