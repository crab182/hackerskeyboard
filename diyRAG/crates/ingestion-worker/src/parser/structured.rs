//! Structured-data parser for `json` (`serde_json`) and `eml` (`mail-parser`)
//! (MASTER_BUILD_SPEC.md §6.3).
//!
//! * **JSON** is flattened to readable `path = value` lines (bounded recursion
//!   depth, §12.4) and emitted as Triple blocks the chunker keeps intact (§6.4).
//! * **Email** yields a header block (Subject/From/To/Date) + the text body.
//!
//! All extracted text is untrusted (§12.5) and passes through
//! [`MarkupParser::sanitize_text`] before it becomes a block.

use async_trait::async_trait;

use super::markup::MarkupParser;
use super::{
    BlobRef, Block, Confidence, MimeSniff, ParseError, ParseOpts, Parser, StructureType,
    StructuredDoc,
};

/// Hard cap on JSON nesting walked, so a deeply/cyclically nested document can't
/// blow the stack or fan out unboundedly (parser-bomb guard, §12.4).
const MAX_JSON_DEPTH: usize = 64;
/// Flattened JSON lines per Triple block (keeps a block bounded for the chunker).
const JSON_LINE_WINDOW: usize = 100;

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

    async fn parse(&self, blob: BlobRef, _opts: &ParseOpts) -> Result<StructuredDoc, ParseError> {
        let blocks = if is_json(&blob) {
            parse_json(&blob.bytes)?
        } else {
            parse_eml(&blob.bytes)?
        };
        if blocks.is_empty() {
            return Err(ParseError::Permanent(
                "no content extracted from structured document".into(),
            ));
        }
        Ok(StructuredDoc {
            blocks,
            lang: None,
            page_count: None,
        })
    }
}

/// JSON vs email dispatch: declared extension first, then a content sniff (a JSON
/// document's first non-whitespace byte is `{` or `[`).
fn is_json(blob: &BlobRef) -> bool {
    let ext = blob
        .declared_name
        .rsplit('.')
        .next()
        .filter(|e| !e.is_empty() && *e != blob.declared_name)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("json") => true,
        Some("eml") => false,
        _ => matches!(
            blob.bytes.iter().find(|b| !b.is_ascii_whitespace()),
            Some(b'{') | Some(b'[')
        ),
    }
}

/// JSON → flattened `path = value` Triple blocks (windowed).
fn parse_json(bytes: &[u8]) -> Result<Vec<Block>, ParseError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| ParseError::Permanent(format!("json: {e}")))?;
    let mut lines = Vec::new();
    flatten_json(&value, "", 0, &mut lines);
    let lines: Vec<String> = lines
        .into_iter()
        .map(|l| MarkupParser::sanitize_text(&l).trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Err(ParseError::Permanent("empty json document".into()));
    }
    Ok(lines
        .chunks(JSON_LINE_WINDOW)
        .map(|w| Block {
            text: w.join("\n"),
            structure_type: StructureType::Triple,
            section_heading: None,
            page_number: None,
        })
        .collect())
}

/// Recursively flatten a JSON value into `path = scalar` lines, dotted for object
/// keys and `[i]`-indexed for arrays. Bounded by [`MAX_JSON_DEPTH`].
fn flatten_json(value: &serde_json::Value, path: &str, depth: usize, out: &mut Vec<String>) {
    use serde_json::Value;
    if depth > MAX_JSON_DEPTH {
        out.push(format!("{path} = …(max depth)"));
        return;
    }
    let here = if path.is_empty() { "$" } else { path };
    match value {
        Value::Object(map) if map.is_empty() => out.push(format!("{here} = {{}}")),
        Value::Object(map) => {
            for (k, v) in map {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                flatten_json(v, &child, depth + 1, out);
            }
        }
        Value::Array(arr) if arr.is_empty() => out.push(format!("{here} = []")),
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                flatten_json(v, &format!("{path}[{i}]"), depth + 1, out);
            }
        }
        Value::String(s) => out.push(format!("{here} = {s}")),
        Value::Number(n) => out.push(format!("{here} = {n}")),
        Value::Bool(b) => out.push(format!("{here} = {b}")),
        Value::Null => out.push(format!("{here} = null")),
    }
}

/// Email → header block (Subject/From/To/Date) + text body block. Attachment
/// recursion through the router is a tracked follow-up (§6.3).
fn parse_eml(bytes: &[u8]) -> Result<Vec<Block>, ParseError> {
    use mail_parser::MessageParser;

    let msg = MessageParser::default()
        .parse(bytes)
        .ok_or_else(|| ParseError::Permanent("could not parse email".into()))?;

    let subject = msg
        .subject()
        .map(|s| MarkupParser::sanitize_text(s).trim().to_owned());
    let heading = subject.clone().filter(|s| !s.is_empty());

    let mut header = String::new();
    if let Some(s) = &subject {
        header.push_str(&format!("Subject: {s}\n"));
    }
    let from = render_address(msg.from());
    if !from.is_empty() {
        header.push_str(&format!("From: {from}\n"));
    }
    let to = render_address(msg.to());
    if !to.is_empty() {
        header.push_str(&format!("To: {to}\n"));
    }
    if let Some(date) = msg.date() {
        header.push_str(&format!("Date: {}\n", date.to_rfc3339()));
    }

    let mut blocks = Vec::new();
    let header = MarkupParser::sanitize_text(&header).trim().to_owned();
    if !header.is_empty() {
        blocks.push(Block {
            text: header,
            structure_type: StructureType::Prose,
            section_heading: heading.clone(),
            page_number: None,
        });
    }
    if let Some(body) = msg.body_text(0) {
        let body = MarkupParser::sanitize_text(&body).trim().to_owned();
        if !body.is_empty() {
            blocks.push(Block {
                text: body,
                structure_type: StructureType::Prose,
                section_heading: heading,
                page_number: None,
            });
        }
    }
    Ok(blocks)
}

/// Render the first address of a header (`Name <addr>` / `addr` / `Name`), or "".
fn render_address(address: Option<&mail_parser::Address>) -> String {
    match address.and_then(mail_parser::Address::first) {
        Some(addr) => match (addr.name(), addr.address()) {
            (Some(name), Some(email)) => format!("{name} <{email}>"),
            (None, Some(email)) => email.to_owned(),
            (Some(name), None) => name.to_owned(),
            (None, None) => String::new(),
        },
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn flatten(v: serde_json::Value) -> Vec<String> {
        let mut out = Vec::new();
        flatten_json(&v, "", 0, &mut out);
        out
    }

    #[test]
    fn flatten_json_dots_objects_and_indexes_arrays() {
        let v = serde_json::json!({
            "name": "Ada",
            "tags": ["x", "y"],
            "meta": { "active": true, "score": 9 },
            "empty_obj": {},
            "empty_arr": [],
            "nothing": null,
        });
        let lines = flatten(v);
        assert!(lines.contains(&"name = Ada".to_string()));
        assert!(lines.contains(&"tags[0] = x".to_string()));
        assert!(lines.contains(&"tags[1] = y".to_string()));
        assert!(lines.contains(&"meta.active = true".to_string()));
        assert!(lines.contains(&"meta.score = 9".to_string()));
        assert!(lines.contains(&"empty_obj = {}".to_string()));
        assert!(lines.contains(&"empty_arr = []".to_string()));
        assert!(lines.contains(&"nothing = null".to_string()));
    }

    #[test]
    fn flatten_json_bounds_depth() {
        // Build a chain deeper than MAX_JSON_DEPTH and confirm it terminates with
        // the sentinel rather than recursing without bound.
        let mut v = serde_json::json!("leaf");
        for _ in 0..(MAX_JSON_DEPTH + 5) {
            v = serde_json::json!({ "n": v });
        }
        let lines = flatten(v);
        assert!(
            lines.iter().any(|l| l.contains("max depth")),
            "got {lines:?}"
        );
    }

    #[tokio::test]
    async fn parses_json_into_a_triple_block() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(br#"{"a": 1, "b": "two"}"#),
            declared_name: "doc.json".into(),
        };
        let doc = StructuredParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .expect("json parses");
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.blocks[0].structure_type, StructureType::Triple);
        assert!(doc.blocks[0].text.contains("a = 1"));
        assert!(doc.blocks[0].text.contains("b = two"));
    }

    #[tokio::test]
    async fn invalid_json_is_a_permanent_error() {
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"{not valid"),
            declared_name: "bad.json".into(),
        };
        let err = StructuredParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ParseError::Permanent(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn parses_eml_headers_and_body() {
        let raw = b"From: Ada <ada@example.com>\r\n\
                    To: Bob <bob@example.com>\r\n\
                    Subject: Hello\r\n\
                    Date: Wed, 18 Jun 2025 10:00:00 +0000\r\n\
                    \r\n\
                    This is the body text.\r\n";
        let blob = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(raw),
            declared_name: "msg.eml".into(),
        };
        let doc = StructuredParser::new()
            .parse(blob, &ParseOpts::default())
            .await
            .expect("eml parses");
        let all: String = doc
            .blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("Subject: Hello"), "got: {all}");
        assert!(all.contains("From: Ada <ada@example.com>"));
        assert!(all.contains("This is the body text."));
        // Subject is carried as the section heading.
        assert!(doc
            .blocks
            .iter()
            .any(|b| b.section_heading.as_deref() == Some("Hello")));
    }

    #[test]
    fn json_vs_eml_dispatch() {
        let j = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"   {\"a\":1}"),
            declared_name: "blob".into(), // no ext → sniff
        };
        assert!(is_json(&j));
        let e = BlobRef {
            key: "k".into(),
            bytes: Bytes::from_static(b"From: a@b.c\r\nSubject: x\r\n\r\nbody"),
            declared_name: "blob".into(),
        };
        assert!(!is_json(&e));
    }
}
