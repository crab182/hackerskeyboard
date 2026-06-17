# ADR-0002: Python confined to inference & hard parsing
- Status: Accepted
- Date: 2026-06-17

## Context
Rust owns the service tier (ADR-0001), but two capabilities have no production-grade Rust equivalent in 2026: (1) high-throughput LLM serving and certain embedding/OCR models exposed only through the PyTorch/transformers ecosystem (vLLM, FlagEmbedding/BGE-M3, bge-reranker), and (2) deep-learning document AI for scanned/complex layouts (Docling, Surya, Marker).

## Decision
Confine Python to exactly **two deployable services**, each behind a stable gRPC/HTTP interface so it is swappable and independently scalable:
1. **`gpu-runtime`** (`services-py/gpu-runtime`) — `/embed`, `/rerank`, `/infer`, `/ocr`. Optional: the default backend is Rust-native (ADR-0004); the Python backend is selected on Linux/CUDA throughput nodes.
2. **`parsing-service`** (`services-py/parsing-service`) — `parse_hard()` for scanned/complex documents only.

The Rust `ingestion-worker` handles every clean/digital format with native crates and spawns LibreOffice/Calibre as sandboxed child processes itself — Python is **not** in the LibreOffice/Calibre path. The common, well-formed ingestion path involves no Python process at all.

## Consequences
**Easier:** best-in-class ML quality where it matters; Python blast radius is small, isolated, and individually replaceable; the hot path stays Rust.

**Harder:** two language toolchains and two extra Dockerfiles; a gRPC/HTTP contract to version between Rust callers and Python services; OCR/complex-parse latency crosses a process boundary.

**Follow-ups:** pin Python deps (`uv.lock`/`requirements.txt`) and model hashes; run both sidecars non-root; make the Rust↔Python contract part of the integration test suite; allow a zero-Python deployment (ADR-0004) with documented quality trade-offs.

## Alternatives considered
- **All-Python** — simplest ML story, rejected per ADR-0001.
- **Zero-Python (Rust ONNX/`ocrs` only)** — possible but materially worse on scanned/complex documents; offered as an opt-in profile, not the default.
