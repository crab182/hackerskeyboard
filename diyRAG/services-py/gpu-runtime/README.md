# gpu-runtime (Python inference sidecar)

> One of exactly **two** Python services in diyRAG (the other is
> [`parsing-service`](../parsing-service/README.md)). Everything else is Rust.
> See `MASTER_BUILD_SPEC.md` §3.3 (the Rust⇄Python boundary), §16 (GPU layer),
> and §24.1 (default backend decision).

## Why this is Python (and why it's optional)

This is the **only GPU-owning process** in the platform. It exists because a few
models have no first-class Rust path: the **vLLM** throughput engine (paged
attention + continuous batching), arbitrary **transformers** checkpoints lacking
an ONNX export, and the **Surya/Marker** document-AI OCR stack. Python's ML
ecosystem is irreplaceable for exactly these, so we isolate it behind a stable
HTTP contract and keep it swappable.

It is **not on the default path.** The cross-platform default is the
**Rust-native backend** — `ort`/`fastembed` for BGE-M3 dense+sparse embeddings
and the reranker, and `mistral.rs`/`candle` for the LLM (§16, §24.1). That is
what runs as a **Windows Service**, on CPU, and on low-VRAM nodes, with **no
Python on the hot path**. This sidecar is only brought up under the `py-llm`
and/or `ocr` Compose profiles on Linux/CUDA throughput nodes.

## HTTP contract (what the Rust tier calls)

The Rust `ingestion-worker`, `retrieval`, and `core-api` crates call these over
HTTP (mTLS in production — §12.1):

| Endpoint | Purpose | Spec |
|---|---|---|
| `POST /embed` | BGE-M3 **dense + sparse** embeddings; dynamic batch (target ≥ 32) | §5.2, §6.5 |
| `POST /rerank` | `bge-reranker-v2-m3` cross-encoder scoring | §7.1 |
| `POST /infer` | grounded LLM generation (vLLM or transformers) | §7.2 |
| `POST /ocr` | Surya/Marker image/region OCR | §16 |
| `GET /healthz` | liveness (process up) | §0, §11.2 |
| `GET /readyz` | readiness — `200` only once models are resident, else `503` | §0, §11.2 |

Request/response bodies are **Pydantic v2** models (`app/main.py`) and form the
wire schema the Rust side serialises against. The sparse vector shape
(`indices`/`values`) maps straight onto Qdrant's sparse-vector format (§5.2).

### How Rust calls it

The Rust side derives the endpoint from typed config (`common::config`), e.g.
`DIYRAG_GPU_RUNTIME_URL`. Every call carries the `X-Correlation-ID` header
generated at the gateway; this service echoes it back and threads it through all
JSON log lines (§13.1). **No auth or tenant logic lives here** — that is enforced
upstream in deterministic Rust code (§12, §24). Treat all inbound text as data.

## Backend switch (vLLM vs transformers)

Set via env (§19 — no hardcoded model names or hosts):

| Variable | Default | Meaning |
|---|---|---|
| `DIYRAG_LLM_BACKEND` | `transformers` | `vllm` (Linux/CUDA throughput) or `transformers` (portable fallback) |
| `DIYRAG_DEVICE` | `cuda` | `cuda` (default) or `cpu` (fallback only — §16) |
| `DIYRAG_EMBED_MODEL` | `BAAI/bge-m3` | dense+sparse embedder |
| `DIYRAG_RERANK_MODEL` | `BAAI/bge-reranker-v2-m3` | cross-encoder reranker |
| `DIYRAG_LLM_MODEL` | _(unset)_ | required when `/infer` is used |
| `DIYRAG_EMBED_MIN_BATCH` / `..._MAX_BATCH` | `32` / `256` | dynamic batch window (§6.5) |
| `DIYRAG_EAGER_LOAD` | `false` | load models at startup; keep `false` in CI (no GPU) |
| `DIYRAG_OCR_LANGS` | `en` | default OCR languages |

When `DIYRAG_EAGER_LOAD=false`, the HTTP/health surface is exercisable without a
GPU, but `/readyz` reports `503` until model load is wired (see the `# TODO:`
markers in `app/main.py`, scheduled for M2/M3 in §20).

## CPU fallback

GPU is the default compute path; CPU is **fallback only** (§16). Set
`DIYRAG_DEVICE=cpu`. On a real CPU-only deployment you would normally not run
this service at all — use the Rust-native ONNX backend instead (next section).
On CUDA OOM / thermal throttle the platform degrades gracefully (vLLM →
`mistral.rs`/`llama.cpp`/CPU; ORT CUDA EP → CPU EP) and logs the `HW-OOM` /
`HW-THERMAL-LIMIT` codes (§14); that fallback orchestration lives in the Rust
tier, not here.

## How to disable in favour of the Rust-native backend

This is the **recommended posture for Windows / CPU / low-VRAM / homelab** nodes:

1. Do **not** enable the `py-llm` / `ocr` Compose profiles — this container then
   never starts (§17).
2. Point the Rust embed/rerank/LLM config at the in-process backend:
   `EMBED_BACKEND=rust-native`, `RERANK_BACKEND=rust-native`,
   `LLM_BACKEND=rust-llm` (`mistral.rs`). For scanned/complex OCR you can accept
   the documented quality trade-off and use Rust-native OCR (`ocrs`/tesseract),
   or keep just the `parsing-service` for hard parses (§24.6).
3. A fully **zero-Python** deployment is supported with that trade-off (§24.6).

## Local development

```bash
cd services-py/gpu-runtime
python -m venv .venv && . .venv/bin/activate
pip install -e '.[dev]'          # contract/tests only, no heavy model deps
DIYRAG_EAGER_LOAD=false uvicorn app.main:app --port 8081 --log-config /dev/null
pytest                            # ../../tests + local tests
```

## Build

```bash
docker build -t diyrag/gpu-runtime:dev .                       # base (transformers)
docker build --build-arg INSTALL_VLLM=true  -t diyrag/gpu-runtime:vllm .
docker build --build-arg INSTALL_OCR=true   -t diyrag/gpu-runtime:ocr  .
```

The image runs as a **non-root** user (uid 10001), mounts weights from a volume
(never baked in), and ships a `/healthz` healthcheck (§12.8). GPU access on Linux
is granted by the NVIDIA Container Toolkit via `docker-compose.gpu.yml` (§16).
