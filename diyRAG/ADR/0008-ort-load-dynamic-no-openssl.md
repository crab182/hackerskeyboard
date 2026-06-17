# ADR-0008: ort `load-dynamic` — no OpenSSL, no build-time download
- Status: **Superseded by [ADR-0009](./0009-candle-replaces-ort.md)** (2026-06-17)
- Date: 2026-06-17

> **Superseded.** The Rust-native embed/rerank path no longer uses `ort` at all.
> `ort 2.0.0-rc.x` proved unstable (a VitisAI EP compile break surfaced even with
> `cuda`/`directml` dropped), so the project switched to **candle** (HuggingFace's
> pure-Rust ML framework). candle needs no ONNX Runtime, no `libonnxruntime`, and
> no `ORT_DYLIB_PATH` — it is OpenSSL-free and offline-capable by construction,
> which preserves the goals this ADR set out to protect. The decision and its
> trade-offs (notably: no DirectML) are recorded in ADR-0009. The text below is
> retained for history.

## Context
The Rust-native inference path (ADR-0004) uses `ort` (ONNX Runtime bindings) for BGE-M3 embeddings and the bge-reranker cross-encoder in `retrieval` and `ingestion-worker`. With its default features, `ort` enables `download-binaries`, which fetches a prebuilt ONNX Runtime over the network at build time using `ureq` → `native-tls` → **OpenSSL**.

A Socket Security scan on the committed `Cargo.lock` surfaced `openssl@0.10.81` (+ `openssl-sys`, `native-tls`) in the graph. This violates two binding project principles:
- **rustls everywhere / OpenSSL-free** (§12.1, ADR-0001).
- **local/LAN-only + offline-capable** (the standing default): a build that phones home to download a binary cannot run air-gapped and contradicts the self-hosted posture.

## Decision
Configure `ort` with **`default-features = false`** and the **`load-dynamic`** feature (plus `cuda`, `directml` for EP APIs). `load-dynamic`:
- performs **no build-time download** and **no static link** — it `dlopen`s `libonnxruntime` at **runtime** from `ORT_DYLIB_PATH`;
- therefore pulls **no `ureq`/`native-tls`/OpenSSL** into the dependency graph.

The node **provides `libonnxruntime` itself** — vendored into the Rust service images (`retrieval`, `ingestion-worker`) and/or the model-cache volume, selected per node as a CPU or GPU (CUDA/DirectML) build. `ORT_DYLIB_PATH` (in `.env`) points at it.

## Consequences
- **Easier:** OpenSSL is fully removed (`cargo.lock`: 620 → 606 packages; only the harmless `openssl-probe` cert-path locator used *by rustls* remains); the Rust build is air-gappable (no binary fetch), consistent with `scripts/vendor.sh`; GPU vs CPU is a deployment choice (swap the provided library), not a recompile.
- **Harder:** images/nodes must ship a matching `libonnxruntime` (added to the inference service images + vendor/bootstrap docs); a missing/mismatched library fails at startup rather than build — surface it clearly in `/readyz` and the `456468ann` gate.
- **Follow-up:** add `libonnxruntime` provisioning to the `retrieval`/`ingestion-worker` Dockerfiles and the bootstrap; pin its version alongside the model hashes (§12.9).

## Alternatives considered
- **Keep `download-binaries`, force rustls** — removes OpenSSL but still downloads a binary at build time; breaks offline/LAN-only. Rejected.
- **Drop `ort`, use the Python `gpu-runtime` only** — removes ONNX from the Rust tree but abandons the zero-Python default backend (ADR-0004). Rejected for the default; remains available as the `py-llm`/OCR profile.
