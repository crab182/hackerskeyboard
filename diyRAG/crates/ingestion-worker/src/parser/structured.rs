//! Structured-data parser for `json` (`serde_json`) and `eml` (`mail-parser`)
//! (MASTER_BUILD_SPEC.md §6.3). For email, headers + body are extracted and
//! attachments are recursed back through the [`super::ParserRouter`].

use async_trait::async_trait;

use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

/// Handles `application/json` and `message/rfc822` (`.eml`).
pub struct StructuredParser;

impl StructuredParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StructuredParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for StructuredParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        match sniff.sniffed_mime.as_deref() {
            Some("application/json") | Some("message/rfc822") => Confidence::Yes,
            _ => match sniff.extension.as_deref() {
                Some("json") | Some("eml") => Confidence::Maybe,
                _ => Confidence::No,
            },
        }
    }

    async fn parse(&self, _blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        // TODO: json → serde_json::from_slice into a Value; flatten to readable
        //       text Blocks (path: value), bounded depth (§12.4).
        //       eml → mail_parser::MessageParser; emit header Block + body Block;
        //       recurse attachments through the router (sanitize each, §12.5).
        Err(ParseError::Permanent("structured parsing not yet implemented".into()))
    }
}
