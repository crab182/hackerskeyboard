//! Spreadsheet parser for `xlsx` / `xls` (`calamine`) and `csv` (`csv` crate)
//! (MASTER_BUILD_SPEC.md §6.3). Rows are grouped into row/section chunks.

use async_trait::async_trait;

use super::{BlobRef, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructuredDoc};

const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// Handles `.xlsx`, `.xls`, and `.csv`.
pub struct SpreadsheetParser;

impl SpreadsheetParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SpreadsheetParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for SpreadsheetParser {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence {
        match sniff.sniffed_mime.as_deref() {
            Some(XLSX_MIME) | Some("application/vnd.ms-excel") => Confidence::Yes,
            Some("text/csv") => Confidence::Yes,
            _ => match sniff.extension.as_deref() {
                Some("xlsx") | Some("xls") | Some("csv") => Confidence::Maybe,
                _ => Confidence::No,
            },
        }
    }

    async fn parse(&self, _blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        // TODO: csv → csv::Reader, each row (or N-row window) → Block (Table).
        //       xlsx/xls → calamine::open_workbook_from_rs, iterate worksheets,
        //       emit per-sheet heading + tabular Blocks rendered as markdown.
        Err(ParseError::Permanent(
            "spreadsheet parsing not yet implemented".into(),
        ))
    }
}
