# ADR-0004: Rust-native inference default, vLLM optional
- Status: Accepted
- Date: 2026-06-17

## Context
The platform must run the same release as a Windows Service and as an unraid container. vLLM — the throughput-leading LLM server — is effectively Linux/CUDA-only and cannot anchor the Windows runtime. Meanwhile, Rust now has credible inference: **candle** (HuggingFace's pure-Rust ML framework) for embeddings + cross-encoder reranking, and `mistral.rs` (itself candle-based) for LLM generation.

> **Update (ADR-0009):** the embed/rerank backend was first specified as `ort`/`fastembed` (ONNX Runtime). `ort 2.0.0-rc.x` proved unstable and pulled ONNX/`libonnxruntime` provisioning into every node, so it was replaced by **candle**. This ADR's decision still holds — *Rust-native is the default* — only the embed/rerank engine changed. See ADR-0009.

## Decision
Make the **Rust-native backend the default**: **candle** (XLM-RoBERTa: BGE-M3 dense embeddings + the bge-reranker-v2-m3 cross-encoder) and `mistral.rs`/`candle` for answer generation. It runs on Windows, Linux, and macOS, CPU or GPU. Expose **vLLM (Python `gpu-runtime`) as an opt-in profile** (`py-llm`) auto-selected on Linux/CUDA throughput nodes; the same Python path also supplies BGE-M3's **learned-sparse** vectors, which have no candle head (ADR-0009). Both sit behind the same `gpu-runtime` gRPC/HTTP contract and the Rust `EmbeddingBackend`/`LlmBackend` traits, so a node picks a backend by config without code changes.

## Consequences
**Easier:** one release runs everywhere incl. Windows-as-a-service; no Python (and no ONNX Runtime) required for a basic CPU/CUDA node; consistent embeddings when the same model weights are used across the fleet.

**Harder:** vLLM still wins on raw multi-tenant throughput, so high-load Linux nodes will opt into Python; must record `embed_model` per chunk to detect cross-node drift (ties into ADR-0005 sync); candle has **no DirectML** path, so dense-only Rust acceleration on Windows is CUDA-only (AMD/Intel Windows GPUs fall back to CPU or the Python `gpu-runtime`); BGE-M3 **sparse** retrieval requires the Python `gpu-runtime` until a Rust sparse head exists.

**Follow-ups:** the retrieval eval harness (`/eval`) gates any backend/model swap; document GPU EP selection on Windows (`deploy/windows/GPU.md`).

## Alternatives considered
- **vLLM everywhere** — best throughput, but breaks the Windows-service requirement. Rejected as the default.
- **External hosted API** — simplest, but violates the self-hosted mission; kept only as a last-resort fallback profile.
- **`ort` (ONNX Runtime) for embed/rerank** — the original choice; rejected after `ort 2.0.0-rc.x` instability and the `libonnxruntime` provisioning burden it imposed on every node. See ADR-0009.
