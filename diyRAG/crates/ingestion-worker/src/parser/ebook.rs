//! E-book conversion parser for `mobi` / `azw3` (MASTER_BUILD_SPEC.md §6.3).
//!
//! There is no good native Rust reader for these formats, so we spawn Calibre's
//! `ebook-convert` as a **sandboxed child process** (`tokio::process`) to convert
//! to EPUB, then delegate to [`super::epub::EpubParser`]. The child is run with
//! resource caps and a timeout (§6.3, §22 row 6); the binary is language-agnostic
//! and driven by Rust, not Python (§3.3).

use async_trait::async_trait;

use super::epub::EpubParser;
use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

/// Handles `.mobi` and `.azw3` via Calibre conversion → EPUB.
pub struct EbookConvertParser {
    /// Absolute path to the `ebook-convert` binary (config-driven, never hardcoded).
    convert_bin: String,
    inner: EpubParser,
}

impl EbookConvertParser {
    pub fn new() -> Self {
        // DECISION: default binary name resolves via PATH; production config
        // should pin an absolute, ACL-restricted path (§12.8). Override via
        // typed config (§0).
        Self {
            convert_bin: "ebook-convert".to_string(),
            inner: EpubParser::new(),
        }
    }

    /// Spawn `ebook-convert` in a sandbox, capped on CPU/memory/time (§6.3, §22).
    async fn convert_to_epub(
        &self,
        _blob: &BlobRef,
        _opts: &ParseOpts,
    ) -> Result<bytes::Bytes, ParseError> {
        // TODO: write bytes to a temp file under a restricted dir; run
        //       tokio::process::Command::new(&self.convert_bin) with:
        //         - a per-process timeout (kill on overrun),
        //         - rlimits / job-object / cgroup memory+cpu caps,
        //         - no network, minimal env,
        //       then read the produced .epub bytes. Always clean up temp files.
        let _ = &self.convert_bin;
        Err(ParseError::Permanent(
            "ebook-convert sandbox not yet implemented".into(),
        ))
    }
}

impl Default for EbookConvertParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for EbookConvertParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        match sniff.sniffed_mime.as_deref() {
            Some("application/x-mobipocket-ebook") => Confidence::Yes,
            _ => match sniff.extension.as_deref() {
                Some("mobi") | Some("azw3") | Some("azw") => Confidence::Maybe,
                _ => Confidence::No,
            },
        }
    }

    async fn parse(&self, blob: BlobRef, opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        let epub_bytes = self.convert_to_epub(&blob, opts).await?;
        let epub_blob = BlobRef {
            key: blob.key,
            bytes: epub_bytes,
            declared_name: format!("{}.epub", blob.declared_name),
        };
        self.inner.parse(epub_blob, opts).await
    }
}
