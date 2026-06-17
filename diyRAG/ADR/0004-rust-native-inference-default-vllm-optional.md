# ADR-0004: Rust-native inference default, vLLM optional
- Status: Accepted
- Date: 2026-06-17

## Context
The platform must run the same release as a Windows Service and as an unraid container. vLLM — the throughput-leading LLM server — is effectively Linux/CUDA-only and cannot anchor the Windows runtime. Meanwhile, Rust now has credible inference: `ort` (ONNX Runtime, with CUDA/TensorRT/DirectML/CoreML execution providers), `fastembed` for embeddings, and `mistral.rs`/`candle` for LLM generation.

## Decision
Make the **Rust-native backend the default**: `ort`/`fastembed` for BGE-M3 dense+sparse embeddings and the bge-reranker cross-encoder; `mistral.rs`/`candle` for answer generation. It runs on Windows and Linux, CPU or GPU. Expose **vLLM (Python `gpu-runtime`) as an opt-in profile** (`py-llm`) auto-selected on Linux/CUDA throughput nodes. Both sit behind the same `gpu-runtime` gRPC/HTTP contract and the Rust `EmbeddingBackend`/`LlmBackend` traits, so a node picks a backend by config without code changes.

## Consequences
**Easier:** one release runs everywhere incl. Windows-as-a-service; no Python required for a basic GPU node; consistent embeddings if the same model/ONNX export is used across the fleet.

**Harder:** vLLM still wins on raw multi-tenant throughput, so high-load Linux nodes will opt into Python; must record `embed_model` per chunk to detect cross-node drift (ties into ADR-0005 sync); ONNX exports must be maintained for the chosen models.

**Follow-ups:** the retrieval eval harness (`/eval`) gates any backend/model swap; document GPU EP selection on Windows (`deploy/windows/GPU.md`).

## Alternatives considered
- **vLLM everywhere** — best throughput, but breaks the Windows-service requirement. Rejected as the default.
- **External hosted API** — simplest, but violates the self-hosted mission; kept only as a last-resort fallback profile.
