//! Spreadsheet parser for `xlsx` / `xls` (`calamine`) and `csv` (`csv` crate)
//! (MASTER_BUILD_SPEC.md §6.3). Each sheet becomes a section heading + one or more
//! **Table** blocks (a markdown table per row-window), so the structure-aware
//! chunker keeps tabular content intact (§6.4) and bounded.
//!
//! Cell text is untrusted (§12.5): every value is run through
//! [`MarkupParser::sanitize_text`] (drops zero-width / control chars, NFC-norm)
//! before it lands in a block, so hidden-instruction payloads in a cell never
//! reach the chunker or the model.

use async_trait::async_trait;

use super::markup::MarkupParser;
use super::{
    BlobRef, Block, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructureType,
    StructuredDoc,
};

const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// Data rows emitted per Table block. Repeating the header in each window keeps
/// every block self-contained for retrieval while bounding its size (§6.4).
const ROW_WINDOW: usize = 50;

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

    async fn parse(&self, blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        let blocks = if is_workbook(&blob) {
            parse_workbook(&blob.bytes)?
        } else {
            parse_csv(&blob.bytes)?
        };
        if blocks.is_empty() {
            return Err(ParseError::Permanent(
                "no rows extracted from spreadsheet".into(),
            ));
        }
        Ok(StructuredDoc {
            blocks,
            lang: None,
            page_count: None,
        })
    }
}

/// Workbook (binary) vs CSV (text) dispatch: declared extension first, then the
/// container magic bytes (`PK` ZIP for xlsx, OLE header for legacy xls).
fn is_workbook(blob: &BlobRef) -> bool {
    let ext = blob
        .declared_name
        .rsplit('.')
        .next()
        .filter(|e| !e.is_empty() && *e != blob.declared_name)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("xlsx") | Some("xls") => true,
        Some("csv") => false,
        _ => {
            let b = &blob.bytes;
            b.starts_with(b"PK\x03\x04") || b.starts_with(&[0xD0, 0xCF, 0x11, 0xE0])
        }
    }
}

/// Sanitize a cell value (untrusted; §12.5) and flatten embedded newlines so it
/// fits one markdown table cell.
fn clean_cell(raw: &str) -> String {
    MarkupParser::sanitize_text(raw)
        .replace(['\n', '\r'], " ")
        .replace('|', "\\|")
        .trim()
        .to_owned()
}

/// CSV → row-windowed Table blocks. Row 0 is treated as the header.
fn parse_csv(bytes: &[u8]) -> Result<Vec<Block>, ParseError> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false) // we promote row 0 to the header ourselves
        .flexible(true) // ragged rows are tolerated, not fatal
        .from_reader(bytes);

    let mut records: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| ParseError::Permanent(format!("csv parse: {e}")))?;
        records.push(rec.iter().map(clean_cell).collect());
    }
    if records.is_empty() {
        return Err(ParseError::Permanent("empty csv".into()));
    }
    let header = &records[0];
    Ok(windowed_table_blocks(header, &records[1..], None))
}

/// xlsx/xls → per-sheet (Heading + Table blocks). The sheet name is the section
/// heading carried onto its Table blocks (§5.4).
fn parse_workbook(bytes: &[u8]) -> Result<Vec<Block>, ParseError> {
    use calamine::{open_workbook_auto_from_rs, Reader};
    use std::io::Cursor;

    let mut workbook = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|e| ParseError::Permanent(format!("open workbook: {e}")))?;

    let mut blocks = Vec::new();
    for name in workbook.sheet_names() {
        let range = workbook
            .worksheet_range(&name)
            .map_err(|e| ParseError::Permanent(format!("sheet `{name}`: {e}")))?;
        let mut rows = range.rows();
        let Some(header_row) = rows.next() else {
            continue; // empty sheet
        };
        let header: Vec<String> = header_row
            .iter()
            .map(|c| clean_cell(&c.to_string()))
            .collect();
        let data: Vec<Vec<String>> = rows
            .map(|r| r.iter().map(|c| clean_cell(&c.to_string())).collect())
            .collect();

        let heading = MarkupParser::sanitize_text(&name).trim().to_owned();
        blocks.push(Block {
            text: heading.clone(),
            structure_type: StructureType::Heading,
            section_heading: None,
            page_number: None,
        });
        blocks.extend(windowed_table_blocks(&header, &data, Some(heading)));
    }
    Ok(blocks)
}

/// Split `rows` into windows of [`ROW_WINDOW`] and render each (with the header
/// repeated) as a markdown Table block. A header-only sheet still yields one block.
fn windowed_table_blocks(
    header: &[String],
    rows: &[Vec<String>],
    heading: Option<String>,
) -> Vec<Block> {
    let table_block = |text: String| Block {
        text,
        structure_type: StructureType::Table,
        section_heading: heading.clone(),
        page_number: None,
    };
    if rows.is_empty() {
        return vec![table_block(render_markdown_table(header, &[]))];
    }
    rows.chunks(ROW_WINDOW)
        .map(|window| table_block(render_markdown_table(header, window)))
        .collect()
}

/// PURE: render a header + rows as a GitHub-flavored markdown table. Column count
/// is the widest of header/rows; short rows pad with empty cells.
fn render_markdown_table(header: &[String], rows: &[Vec<String>]) -> String {
    let ncols = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let mut out = String::new();
    out.push_str(&render_row(header, ncols));
    out.push('|');
    for _ in 0..ncols {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        out.push_str(&render_row(row, ncols));
    }
    out.trim_end().to_owned()
}

/// PURE: render one `| a | b | c |` row padded/truncated to `ncols` columns.
fn render_row(cells: &[String], ncols: usize) -> String {
    let mut s = String::from("|");
    for i in 0..ncols {
        s.push(' ');
        s.push_str(cells.get(i).map(String::as_str).unwrap_or(""));
        s.push_str(" |");
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clean_cell_sanitizes_escapes_and_flattens() {
        // zero-width stripped, pipe escaped, newline flattened.
        assert_eq!(clean_cell("a\u{200B}b"), "ab");
        assert_eq!(clean_cell("x | y"), "x \\| y");
        assert_eq!(clean_cell("line1\nline2"), "line1 line2");
    }

    #[test]
    fn render_markdown_table_has_header_separator_and_padded_rows() {
        let md = render_markdown_table(&row(&["a", "b"]), &[row(&["1", "2"]), row(&["3"])]);
        let lines: Vec<&str> = md.lines().collect();
        assert_eq!(lines[0], "| a | b |");
        assert_eq!(lines[1], "| --- | --- |");
        assert_eq!(lines[2], "| 1 | 2 |");
        // Short row is padded to the column count.
        assert_eq!(lines[3], "| 3 |  |");
    }

    #[test]
    fn windowing_splits_large_sheets_and_repeats_header() {
        let header = row(&["c"]);
        let rows: Vec<Vec<String>> = (0..ROW_WINDOW + 5)
            .map(|i| row(&[&i.to_string()]))
            .collect();
        let blocks = windowed_table_blocks(&header, &rows, Some("Sheet1".into()));
        assert_eq!(blocks.len(), 2); // 50 + 5
        for b in &blocks {
            assert_eq!(b.structure_type, StructureType::Table);
            assert_eq!(b.section_heading.as_deref(), Some("Sheet1"));
            assert!(b.text.starts_with("| c |")); // header repeated in each window
        }
    }

    #[test]
    fn header_only_sheet_still_emits_one_table_block() {
        let blocks = windowed_table_blocks(&row(&["only", "header"]), &[], None);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].structure_type, StructureType::Table);
    }

    #[tokio::test]
    async fn parses_csv_into_a_table_block() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"name,score\nAlice,10\nBob,20\n"),
            declared_name: "data.csv".into(),
        };
        let doc = SpreadsheetParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .expect("csv parses");
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.blocks[0].structure_type, StructureType::Table);
        let t = &doc.blocks[0].text;
        assert!(t.contains("| name | score |"), "got: {t}");
        assert!(t.contains("| --- | --- |"));
        assert!(t.contains("| Alice | 10 |"));
        assert!(t.contains("| Bob | 20 |"));
    }

    #[tokio::test]
    async fn csv_cell_with_pipe_is_escaped_end_to_end() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"a,b\n\"x|y\",z\n"),
            declared_name: "p.csv".into(),
        };
        let doc = SpreadsheetParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .unwrap();
        assert!(
            doc.blocks[0].text.contains("x\\|y"),
            "got: {}",
            doc.blocks[0].text
        );
    }

    #[tokio::test]
    async fn empty_csv_is_a_permanent_error() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b""),
            declared_name: "empty.csv".into(),
        };
        let err = SpreadsheetParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ParseError::Permanent(_)), "got {err:?}");
    }

    #[test]
    fn workbook_dispatch_by_extension_and_magic() {
        let xlsx = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"PK\x03\x04 rest"),
            declared_name: "book".into(), // no extension → sniff magic
        };
        assert!(is_workbook(&xlsx));
        let csv = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"a,b\n1,2\n"),
            declared_name: "data.csv".into(),
        };
        assert!(!is_workbook(&csv));
    }
}
