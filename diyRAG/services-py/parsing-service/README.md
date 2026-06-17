# parsing-service (Python hard-parse sidecar)

> The **second and last** Python service in diyRAG (the other is
> [`gpu-runtime`](../gpu-runtime/README.md)). See `MASTER_BUILD_SPEC.md` §3.3
> (Rust⇄Python boundary), §6.3 (parser router), §12.5 / §22 #6 (hostile input).

## Why this is Python

Deep-learning **document AI** has no Rust peer. **Docling** (layout + table
structure) and **Surya/Marker** (GPU OCR) are best-in-class for scanned and
complex/layout-heavy documents. We isolate them behind a stable interface so the
rest of the pipeline stays Rust.

This service is **only invoked when the Rust parser router decides a document is
"hard."** The router runs a cheap text-density heuristic in Rust (§6.3); the
common, well-formed path (digital PDF, DOCX, XLSX, HTML, MD, EPUB, EML, …) is
parsed entirely in Rust with **no Python process involved** (§21 self-QA).
Crossings happen for:

- scanned PDFs / images needing OCR (low text density or `force_ocr`),
- complex layout and table extraction beyond the Rust OOXML/PDF handlers.

**LibreOffice and Calibre conversions are NOT here.** The Rust ingestion-worker
spawns `soffice --headless` / `ebook-convert` as sandboxed child processes
itself (§3.2, §6.3); this service never shells out to them.

## How Rust calls it

The Rust `ingestion-worker`'s `ParserRouter` calls `parse_hard(blob_ref, hints)`
and gets back a normalized **`StructuredDoc`** — exactly what the router expects
from any handler (§6.3):

- `blocks[]`: text blocks with `structure_type` (`prose|table|heading|code|triple`),
  `page_number`, `section_heading`, `bbox` (page coords), `heading_level`,
  `ordinal` (reading order), and OCR `confidence`,
- tables rendered **as markdown** inside a `TABLE` block and kept intact (§6.4),
- `page_count`, `lang`, `parser` (`docling`/`surya`/`marker`), `ocr_used`.

**Transport.** Exposed over **HTTP/JSON (FastAPI)** by default for testability,
with `proto/parsing.proto` mirroring the identical schema so a node can switch to
**gRPC (`tonic` ⇄ grpcio)** without a wire change. The spec permits "gRPC or
FastAPI" (§6.3); HTTP is the more reversible default
(`DECISION:` in `app/main.py`).

| Endpoint | Purpose |
|---|---|
| `POST /parse_hard` | hard parse → `{ doc: StructuredDoc }` or `{ error: ParseError }` |
| `GET /healthz` | liveness |
| `GET /readyz` | readiness — `503` until Docling/OCR engines load |

The `content_sha256` is verified for idempotency/tamper (§6.2). The
`X-Correlation-ID` header is echoed and threaded through JSON logs (§13.1).

## Defensive / hostile-input posture (§12.4, §12.5, §22 #6)

This is a prime target for malicious files, so it is coded defensively:

- **Per-request wall-clock timeout**, clamped to a server ceiling
  (`DIYRAG_PARSE_TIMEOUT_SECS`, default cap 600s).
- **Page and byte caps** bound memory and guard against decompression bombs
  (`DIYRAG_PARSE_MAX_PAGES`, `DIYRAG_PARSE_MAX_BYTES`).
- **Classified failures, never raw 500s.** On timeout/OOM/unsupported the
  service returns a `ParseError` with `classification` = `TRANSIENT` or
  `PERMANENT` (§14), so the Rust worker decides retry-vs-quarantine and **keeps
  processing the batch** (§14 non-stop).
- **Never trust the file:** extracted text is treated as data and never
  executed. Hidden-instruction stripping (zero-width chars, white-on-white, HTML
  comments) happens on the Rust ingest side at §12.5; this service does not
  interpret content.
- Runs as a **non-root** user with a mounted (not baked-in) model cache (§12.8).

## Config (env; no secrets, no hardcoded hosts — §19)

| Variable | Default | Meaning |
|---|---|---|
| `DIYRAG_PARSING_PORT` | `8082` | listen port |
| `DIYRAG_DEVICE` | `cuda` | `cuda` (default) / `cpu` (fallback) |
| `DIYRAG_OCR_LANGS` | `en` | default OCR languages |
| `DIYRAG_PARSE_MAX_PAGES` | `2000` | hard page ceiling |
| `DIYRAG_PARSE_MAX_BYTES` | `536870912` | hard byte ceiling (512 MiB) |
| `DIYRAG_PARSE_TIMEOUT_SECS` | `600` | wall-clock ceiling |
| `DIYRAG_PARSE_DEFAULT_TIMEOUT_SECS` | `120` | default per-request timeout |
| `DIYRAG_EAGER_LOAD` | `false` | load engines at startup; keep `false` in CI |
| `DIYRAG_BLOB_BASE_URL` | _(unset)_ | object_store base for blob fetch (no creds here) |

## Local development

```bash
cd services-py/parsing-service
python -m venv .venv && . .venv/bin/activate
pip install -e '.[dev]'
DIYRAG_EAGER_LOAD=false uvicorn app.main:app --port 8082 --log-config /dev/null
pytest
```

## Build

```bash
docker build -t diyrag/parsing-service:dev .
```

## Disabling (zero-Python deployment)

Don't enable the `parsing`/`ocr` profiles and accept the §24.6 quality trade-off:
the Rust router falls back to Rust-native OCR (`ocrs`/tesseract) and skips
Docling-grade layout/table extraction.
