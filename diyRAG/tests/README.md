# tests/ — cross-cutting test suites

Per-crate unit tests live next to their code (`#[cfg(test)]` in each Rust crate; `pytest` in each Python service). This directory holds the **cross-service** suites from spec §18.

## Taxonomy (spec §18, gates per §21)

| Suite | Location | What it covers |
|---|---|---|
| **Unit** | in-crate (`crates/*/src/**`) + `services-py/*/tests` | parsers (golden files per format incl. epub/mobi/scanned PDF), chunker invariants, auth/RBAC, version-vector resolution, idempotency. |
| **Integration** | `tests/integration/` | ingest→index→retrieve→answer per format; logical-purge filtering; batch with injected failures; crash-and-resume; **service install/start/stop on Windows + Linux** (CI matrix). |
| **E2E** | `tests/e2e/` | Playwright (Chrome + Firefox) + Tauri smoke; help-bubble appears with correct content; error code deep-links to `error_log`. |
| **Load** | `tests/load/` | 25k-file ingestion; 50-user concurrent query; verify SLAs and no starvation. |
| **Security** | `tests/security/` | prompt-injection / RAG-poisoning suite (hidden-text docs must not alter behavior); **cross-tenant isolation** (acceptance #6); rate-limit & authZ; container scan + `cargo-audit`/`cargo-deny`. |
| **Eval** | `../eval/` | nDCG@10 / recall@k / MRR regression gate. |

## Golden files
Each named ingestion format (`pdf, docx, doc, txt, md, rtf, html, epub, mobi, azw3, pptx, xlsx, csv, json, eml`) has a golden-file fixture, including a MOBI (Calibre path) and a scanned PDF (OCR/Python path).

## Running
```bash
just test            # all Rust + Python unit tests
just test-integration
just test-security
just e2e
just load
```

## Acceptance mapping
The suites collectively verify the 9 acceptance criteria (spec §1) and the §21 self-QA checklist — including the dual-runtime criterion #9 (Windows Service reboot-persistence; unraid Compose + CA template; CLI control).

> Scaffolding placeholder — suites are filled in across phases per spec §20; security + isolation gates land in **M6**, service tests in **M9**.
