# ADR-0009: candle replaces ort for in-proc embeddings + reranking
- Status: Accepted
- Date: 2026-06-17
- Supersedes: [ADR-0008](./0008-ort-load-dynamic-no-openssl.md)

## Context
ADR-0004 made a Rust-native backend the default for BGE-M3 embeddings and the
`bge-reranker-v2-m3` cross-encoder; ADR-0008 then pinned `ort` (ONNX Runtime
bindings) to `load-dynamic` so the build stayed OpenSSL-free and offline-capable.

In practice `ort 2.0.0-rc.x` was not stable enough to anchor the default path:

- It is a release-candidate with churn; a **VitisAI execution-provider compile
  error** surfaced in the crate, and dropping `cuda`/`directml` features made the
  standalone build *worse* rather than better.
- `load-dynamic` keeps the graph OpenSSL-free, but it shifts a real burden onto
  every node: each image/host must **provide a matching `libonnxruntime`** at the
  right version (CPU vs CUDA vs DirectML), discovered at runtime via
  `ORT_DYLIB_PATH`. That is a per-node provisioning + version-pinning task that
  cuts against "one release runs everywhere, including the Windows service and
  unraid."
- `fastembed` (the obvious alternative) is itself built on `ort`, so it inherits
  the same instability.

## Decision
Replace `ort` with **candle** — HuggingFace's pure-Rust ML framework — for the
in-process embedding and reranking backends in `retrieval` and `ingestion-worker`.

- Workspace deps: drop `ort`; add `candle-core`, `candle-nn`, `candle-transformers`
  (`tokenizers` stays).
- Models load from a **local directory** (`config.json` + `tokenizer.json` +
  `model.safetensors`) via `DIYRAG_EMBED_MODEL_DIR` / `DIYRAG_RERANK_MODEL_DIR`.
  No Hugging Face Hub fetch at runtime — offline / LAN-only by default.
- Architecture mapping (both bge-m3 models are **XLM-RoBERTa**):
  - Embeddings → `candle_transformers::models::xlm_roberta::XLMRobertaModel`,
    CLS pooling + L2 normalize → the **dense** BGE-M3 vector.
  - Reranker → `XLMRobertaForSequenceClassification` (num_labels = 1) → one
    relevance logit per `(query, passage)` pair.
- Weights load through the **safe** `candle_core::safetensors::load` +
  `VarBuilder::from_tensors` path (not the `unsafe` mmap constructor), so both
  crates keep `#![forbid(unsafe_code)]`.
- **CPU is the default device** so every node builds and boots; **CUDA/Metal**
  are opt-in candle cargo features compiled into a GPU build of the image. Both
  backends still sit behind the existing `EmbeddingBackend` / `RerankBackend`
  traits, and the Python `gpu-runtime` HTTP backend remains the throughput
  alternative — selected by config, no code change (ADR-0004).

## Consequences
- **Easier:** no ONNX Runtime anywhere — **no `libonnxruntime` to vendor, no
  `ORT_DYLIB_PATH`**, nothing to dlopen at startup. Pure-Rust, statically linked,
  OpenSSL-free, and air-gappable by construction (the goals ADR-0008 protected,
  now structural rather than configured). One release runs on Windows, Linux, and
  macOS. candle's tensor tree dropped the rc-grade instability.
- **Harder / trade-offs:**
  - **No DirectML.** candle supports CUDA and Metal, not DirectML. AMD/Intel
    Windows GPUs no longer get in-proc acceleration for embed/rerank; they fall
    back to **CPU** or to the Python `gpu-runtime`. (ONNX Runtime's DirectML EP
    was the only thing that gave those GPUs a Rust-side path; we accept losing it
    for the stability + zero-provisioning win.)
  - **Sparse vectors deferred.** candle's XLM-RoBERTa exposes the encoder + a
    sequence-classification head, but **not** BGE-M3's learned-sparse head. The
    Rust backend therefore emits **dense + rerank** only; the **sparse** signal is
    left empty in-proc and supplied by the Python `gpu-runtime` when sparse/hybrid
    retrieval is enabled. Tracked as a follow-up (a Rust sparse head, or accept
    dense-only hybrid on Rust-only nodes).
  - Model weights must be the **safetensors** export (not ONNX); the model dir is
    vendored into the image / model-cache and version-pinned with the model hash
    (§12.9), exactly as the ONNX export would have been.
- **Follow-up:** validate the candle forward path against a real bge-m3 /
  bge-reranker-v2-m3 checkpoint in the eval harness (`/eval`); the code compiles
  and is unit-tested for the load/unloaded/error paths, but the numeric forward
  pass is not yet verified against a reference (no model ships in CI / the offline
  build). Batch pairs to the VRAM limit (§6.5) — currently one-by-one.

## Alternatives considered
- **Keep `ort` (pin a stable release later)** — `ort 2.x` is still rc; waiting on
  stability blocks the default path and keeps the `libonnxruntime` provisioning
  burden. Rejected.
- **`fastembed`** — built on `ort`; inherits the same instability. Rejected.
- **Python `gpu-runtime` only (drop the Rust embed/rerank backend)** — abandons
  the zero-Python default (ADR-0004) and the Windows-service story. Rejected for
  the default; it remains the opt-in throughput / sparse profile.
