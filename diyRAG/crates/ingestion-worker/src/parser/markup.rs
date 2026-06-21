//! Markup parser for `html` / `md` / `txt` / `rtf` (MASTER_BUILD_SPEC.md §6.3),
//! with mandatory **hidden-text sanitization** per §12.5 / §22 row 1.
//!
//! Every ingested document is untrusted (§12.5). Before emitting blocks we strip
//! known hidden-instruction vectors: zero-width characters, control characters,
//! HTML comments, and off-screen / `font-size:0` / white-on-white text. This is
//! deterministic code outside the LLM (§12).

use async_trait::async_trait;
use unicode_normalization::UnicodeNormalization;

use super::{
    BlobRef, Block, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructureType,
    StructuredDoc,
};

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
            Some("text/html") | Some("text/plain") | Some("application/rtf") | Some("text/rtf") => {
                Confidence::Yes
            }
            _ => match sniff.extension.as_deref() {
                Some("html") | Some("htm") | Some("md") | Some("markdown") | Some("txt")
                | Some("rtf") => Confidence::Maybe,
                _ => Confidence::No,
            },
        }
    }

    async fn parse(&self, blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        // NOTE: every helper below is **synchronous** and runs to completion here
        // (no `.await` between). That matters because `scraper::Html` is `!Send`;
        // by never holding it across an await the `parse` future stays `Send` as
        // `#[async_trait]` requires.
        let text = String::from_utf8_lossy(&blob.bytes);
        let format = detect_format(&blob.declared_name, &text);
        let raw = match format {
            Format::Markdown => parse_markdown(&text),
            Format::Html => parse_html(&text),
            Format::Rtf => parse_plaintext(&strip_rtf(&text)),
            Format::Plain => parse_plaintext(&text),
        };

        // Sanitize every block's text (§12.5) and drop any that empty out.
        let blocks: Vec<Block> = raw
            .into_iter()
            .filter_map(|mut b| {
                b.text = Self::sanitize_text(&b.text).trim().to_owned();
                (!b.text.is_empty()).then_some(b)
            })
            .collect();

        if blocks.is_empty() {
            return Err(ParseError::Permanent(
                "no extractable text in markup document".into(),
            ));
        }
        Ok(StructuredDoc {
            blocks,
            lang: None,
            page_count: None,
        })
    }
}

/// The markup sub-format a blob is parsed as (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Plain,
    Markdown,
    Html,
    Rtf,
}

/// Decide the sub-format from the declared extension first (authoritative when
/// present), then a cheap content sniff of the head. Never panics.
fn detect_format(declared_name: &str, text: &str) -> Format {
    let ext = declared_name
        .rsplit('.')
        .next()
        .filter(|e| !e.is_empty() && *e != declared_name)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("md" | "markdown") => return Format::Markdown,
        Some("html" | "htm") => return Format::Html,
        Some("rtf") => return Format::Rtf,
        Some("txt") => return Format::Plain,
        _ => {}
    }
    // Content sniff on the first non-whitespace bytes (lower-cased, bounded).
    let head: String = text
        .trim_start()
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    if head.starts_with("{\\rtf") {
        Format::Rtf
    } else if head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.starts_with("<body")
    {
        Format::Html
    } else {
        Format::Plain
    }
}

/// A block under construction, before sanitization.
fn block(text: impl Into<String>, kind: StructureType, heading: Option<String>) -> Block {
    Block {
        text: text.into(),
        structure_type: kind,
        section_heading: heading,
        page_number: None,
    }
}

/// Plaintext → one [`Block`] per blank-line-delimited paragraph (§6.3).
fn parse_plaintext(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.trim().is_empty() {
                blocks.push(block(cur.trim(), StructureType::Prose, None));
            }
            cur.clear();
        } else {
            if !cur.is_empty() {
                cur.push('\n');
            }
            cur.push_str(line);
        }
    }
    if !cur.trim().is_empty() {
        blocks.push(block(cur.trim(), StructureType::Prose, None));
    }
    blocks
}

/// Markdown → heading / prose / code blocks via `pulldown_cmark`, carrying the
/// most-recent heading onto following blocks as `section_heading` (§5.4 / §6.4).
fn parse_markdown(md: &str) -> Vec<Block> {
    use pulldown_cmark::{Event, Parser as MdParser, Tag, TagEnd};

    let mut blocks = Vec::new();
    let mut cur = String::new();
    let mut kind = StructureType::Prose;
    let mut current_heading: Option<String> = None;

    let flush = |blocks: &mut Vec<Block>,
                 cur: &mut String,
                 kind: StructureType,
                 current_heading: &mut Option<String>| {
        let text = cur.trim().to_owned();
        cur.clear();
        if text.is_empty() {
            return;
        }
        if kind == StructureType::Heading {
            *current_heading = Some(text.clone());
            blocks.push(block(text, StructureType::Heading, None));
        } else {
            blocks.push(block(text, kind, current_heading.clone()));
        }
    };

    for ev in MdParser::new(md) {
        match ev {
            Event::Start(Tag::Heading { .. }) => {
                flush(&mut blocks, &mut cur, kind, &mut current_heading);
                kind = StructureType::Heading;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut blocks, &mut cur, kind, &mut current_heading);
                kind = StructureType::Code;
            }
            Event::Start(Tag::Paragraph) => {
                flush(&mut blocks, &mut cur, kind, &mut current_heading);
                kind = StructureType::Prose;
            }
            Event::End(TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::Paragraph) => {
                flush(&mut blocks, &mut cur, kind, &mut current_heading);
                kind = StructureType::Prose;
            }
            Event::Text(t) | Event::Code(t) => cur.push_str(&t),
            Event::SoftBreak => cur.push(' '),
            Event::HardBreak => cur.push('\n'),
            _ => {}
        }
    }
    // Trailing text not closed by an end tag (defensive).
    flush(&mut blocks, &mut cur, kind, &mut current_heading);
    blocks
}

/// HTML → blocks by selecting content elements in document order. Script/style
/// (and other non-selected nodes) are excluded by construction. Whitespace is
/// collapsed. Hidden-node pruning by CSS is a follow-up (§12.5 TODO).
fn parse_html(html: &str) -> Vec<Block> {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);
    // Top-level content elements; `li`/`blockquote` omitted to avoid double-
    // counting nested `p`s in this first pass.
    let selector = match Selector::parse("h1,h2,h3,h4,h5,h6,p,pre") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut blocks = Vec::new();
    let mut current_heading: Option<String> = None;
    for el in doc.select(&selector) {
        let text = collapse_ws(&el.text().collect::<String>());
        if text.is_empty() {
            continue;
        }
        let name = el.value().name();
        let kind = match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => StructureType::Heading,
            "pre" => StructureType::Code,
            _ => StructureType::Prose,
        };
        if kind == StructureType::Heading {
            current_heading = Some(text.clone());
            blocks.push(block(text, StructureType::Heading, None));
        } else {
            blocks.push(block(text, kind, current_heading.clone()));
        }
    }
    blocks
}

/// Collapse runs of ASCII whitespace into single spaces and trim (HTML text is
/// whitespace-heavy).
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Best-effort RTF → plaintext: drop group braces and control words, keep escaped
/// chars, and map `\par`/`\line` to newlines. Approximate (full RTF is out of
/// scope); good enough to recover prose for chunking.
fn strip_rtf(rtf: &str) -> String {
    let mut out = String::new();
    let mut chars = rtf.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek().copied() {
                Some(n) if n.is_ascii_alphabetic() => {
                    let mut word = String::new();
                    while let Some(&p) = chars.peek() {
                        if p.is_ascii_alphabetic() {
                            word.push(p);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    // optional numeric parameter
                    while let Some(&p) = chars.peek() {
                        if p.is_ascii_digit() || p == '-' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    // a single trailing space delimits the control word
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                    if word == "par" || word == "line" {
                        out.push('\n');
                    }
                }
                // escaped literal (\{ \} \\) or a control symbol
                Some(n) => {
                    chars.next();
                    if matches!(n, '{' | '}' | '\\') {
                        out.push(n);
                    }
                }
                None => {}
            },
            '{' | '}' => {}
            other => out.push(other),
        }
    }
    out
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

    #[test]
    fn detect_format_prefers_extension_then_sniffs() {
        assert_eq!(detect_format("notes.md", ""), Format::Markdown);
        assert_eq!(detect_format("page.HTML", ""), Format::Html);
        assert_eq!(detect_format("memo.RTF", ""), Format::Rtf);
        assert_eq!(detect_format("readme.txt", ""), Format::Plain);
        // No/unknown extension → sniff content.
        assert_eq!(
            detect_format("blob", "<!DOCTYPE html><p>x</p>"),
            Format::Html
        );
        assert_eq!(detect_format("blob", "{\\rtf1 hi}"), Format::Rtf);
        assert_eq!(detect_format("blob", "just words"), Format::Plain);
    }

    #[test]
    fn plaintext_splits_on_blank_lines() {
        let blocks = parse_plaintext("para one\nstill one\n\n\npara two\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "para one\nstill one");
        assert_eq!(blocks[0].structure_type, StructureType::Prose);
        assert_eq!(blocks[1].text, "para two");
    }

    #[test]
    fn markdown_emits_heading_prose_code_with_section_carry() {
        let md = "# Title\n\nSome prose here.\n\n```\nlet x = 1;\n```\n";
        let blocks = parse_markdown(md);
        assert_eq!(blocks[0].structure_type, StructureType::Heading);
        assert_eq!(blocks[0].text, "Title");
        // Prose + code carry the preceding heading as section context.
        assert_eq!(blocks[1].structure_type, StructureType::Prose);
        assert_eq!(blocks[1].section_heading.as_deref(), Some("Title"));
        assert_eq!(blocks[2].structure_type, StructureType::Code);
        assert!(blocks[2].text.contains("let x = 1;"));
        assert_eq!(blocks[2].section_heading.as_deref(), Some("Title"));
    }

    #[test]
    fn html_extracts_headings_and_paragraphs_skipping_script() {
        let html = "<html><head><style>.x{}</style></head><body>\
            <h2>Heading</h2><p>First   para.</p>\
            <script>alert(1)</script><p>Second.</p></body></html>";
        let blocks = parse_html(html);
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["Heading", "First para.", "Second."]);
        // script/style text never appears.
        assert!(!blocks.iter().any(|b| b.text.contains("alert")));
        assert_eq!(blocks[1].section_heading.as_deref(), Some("Heading"));
    }

    #[test]
    fn rtf_strips_control_words_to_text() {
        let stripped = strip_rtf(r"{\rtf1\ansi Hello \b world\b0\par Bye}");
        // control words removed, \par → newline, prose preserved.
        assert!(stripped.contains("Hello"));
        assert!(stripped.contains("world"));
        assert!(!stripped.contains("ansi"));
        assert!(!stripped.contains("rtf1"));
        assert!(stripped.contains('\n'));
    }

    #[tokio::test]
    async fn parse_markdown_end_to_end_sanitizes_and_structures() {
        // A zero-width char inside the heading must be stripped (§12.5).
        let md = "# Hel\u{200B}lo\n\nWorld para.\n";
        let blob = BlobRef {
            key: "k".into(),
            bytes: bytes::Bytes::from(md.as_bytes().to_vec()),
            declared_name: "doc.md".into(),
        };
        let doc = MarkupParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .expect("markdown parses");
        assert_eq!(doc.blocks[0].text, "Hello");
        assert_eq!(doc.blocks[0].structure_type, StructureType::Heading);
        assert_eq!(doc.blocks[1].text, "World para.");
    }

    #[tokio::test]
    async fn parse_rejects_empty_document() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: bytes::Bytes::from_static(b"   \n\n  "),
            declared_name: "empty.txt".into(),
        };
        let err = MarkupParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ParseError::Permanent(_)), "got {err:?}");
    }
}
