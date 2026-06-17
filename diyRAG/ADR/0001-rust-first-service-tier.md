# ADR-0001: Rust-first service tier
- Status: Accepted
- Date: 2026-06-17

## Context
The original master spec (v1.0) used Python 3.12 + FastAPI for every backend service. The re-plan brief is to build "in Rust, mostly, and Python when it's best suited." The platform has hard latency SLAs (p95 retrieval < 800 ms, p95 answer < 6 s under 50 concurrent users), runs tens of thousands of ingestion work units, must run as a small-footprint native Windows Service, and is a long-lived self-hosted appliance.

## Decision
Implement the entire service tier in **Rust**: `api-gateway`, `core-api`, `retrieval`, `ingestion-worker`, `sync-agent`, `mcp-server`, `autoscaler`, the `diyragd` supervisor, the `diyrag` CLI, and the Tauri native shell. Standardize on **Tokio** (runtime), **Axum + Tower** (HTTP), **sqlx** (Postgres, compile-time-checked queries), **qdrant-client**, **async-nats**, **object_store**, **tonic + rustls** (mTLS gRPC), and **tracing** (observability).

## Consequences
**Easier:** single static binary per service → tiny `scratch`/`distroless` images; no GC pauses → predictable tail latency; memory safety removes buffer-overflow/UAF/data-race CVE classes by construction; one toolchain (`cargo`) for build/test/lint; trivial cross-compile to a Windows service binary; `#![forbid(unsafe_code)]` is enforceable in CI.

**Harder:** higher upfront ramp on ownership/borrow-checker and async Rust; the ML/LLM ecosystem is thinner than Python's (addressed by ADR-0002/0004 — Python sidecars + `ort`/`mistral.rs`); some libraries (Docling, Surya/Marker) have no Rust peer.

**Follow-ups:** keep every external boundary behind a trait (`Parser`, `EmbeddingBackend`, `LlmBackend`, `VectorStore`, `ServiceManager`) so components stay swappable; gate CI on `fmt` + `clippy -D warnings` + `cargo-deny` + `cargo-audit`.

## Alternatives considered
- **Keep Python/FastAPI** — fastest to write, best ML fit, but worse tail latency, heavier images, weaker memory safety, and a poor fit for a native Windows Service. Rejected for the service tier; retained only for the two sidecars.
- **Go for the gateway only** — high throughput, but a second language for marginal benefit; Rust covers it.
