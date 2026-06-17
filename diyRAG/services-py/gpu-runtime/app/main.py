"""diyRAG gpu-runtime — FastAPI inference sidecar (MASTER_BUILD_SPEC.md §3.3, §13, §16).

This is the ONLY GPU-owning process in the platform and the ONLY place vLLM /
transformers / Surya-Marker OCR run. It exposes a stable HTTP contract that the
Rust tier (`ingestion-worker`, `retrieval`, `core-api`) calls:

    POST /embed    dense + sparse BGE-M3 embeddings (§5.2, §6.5)
    POST /rerank   bge-reranker-v2-m3 cross-encoder scoring (§7.1)
    POST /infer    grounded LLM generation (vLLM or transformers) (§7.2)
    POST /ocr       Surya / Marker OCR for image/region OCR (§16)
    GET  /healthz   liveness  (§0, §11.2)
    GET  /readyz    readiness — true only once models are loaded (§0, §11.2)

IMPORTANT — this service is OPTIONAL. The Rust-native backend (`ort`/`fastembed`
for embeddings+rerank, `mistral.rs`/`candle` for the LLM) is the cross-platform
DEFAULT and is what runs as a Windows Service / on CPU / low-VRAM nodes
(§16, §24.1). This container is only started under the `py-llm` / `ocr` compose
profiles. See README.md for how to disable it entirely in favour of Rust-native.

Security posture: this process performs NO access control. AuthN/AuthZ and tenant
isolation are enforced upstream in the Rust `api-gateway` / `core-api`
(deterministic code outside the model — §12, §24). Treat all inbound text as
data, never as instructions; the Rust caller is responsible for the
trusted/untrusted delimiting at generation time (§12.5).
"""

from __future__ import annotations

import logging
import os
import time
import uuid
from contextlib import asynccontextmanager
from enum import Enum
from typing import Any, AsyncIterator

from fastapi import FastAPI, Request, Response
from fastapi.responses import JSONResponse
from pydantic import BaseModel, Field

from .logging_config import configure_json_logging, CORRELATION_HEADER

LOG = logging.getLogger("diyrag.gpu_runtime")

# ---------------------------------------------------------------------------
# Configuration (12-factor; env-driven — §0, §19). NO secrets, NO hardcoded
# model names baked into logic. Model ids are configurable so a node can pin a
# different checkpoint without a code change.
# ---------------------------------------------------------------------------


class Settings:
    """Typed view over the environment. Mirrors the Rust `common::config` style."""

    # Backend switch: "vllm" (Linux/CUDA throughput profile) vs "transformers"
    # (portable fallback for models lacking a vLLM/ONNX path). §16, §24.1.
    LLM_BACKEND: str = os.getenv("DIYRAG_LLM_BACKEND", "transformers").lower()

    # Device: "cuda" (default GPU path) or "cpu" (fallback only — §16).
    DEVICE: str = os.getenv("DIYRAG_DEVICE", "cuda").lower()

    # Configurable model ids (§19: no hardcoded model names).
    EMBED_MODEL: str = os.getenv("DIYRAG_EMBED_MODEL", "BAAI/bge-m3")
    RERANK_MODEL: str = os.getenv("DIYRAG_RERANK_MODEL", "BAAI/bge-reranker-v2-m3")
    LLM_MODEL: str = os.getenv("DIYRAG_LLM_MODEL", "")  # required when /infer used
    OCR_LANGS: str = os.getenv("DIYRAG_OCR_LANGS", "en")

    # Dynamic batch sizing (§6.5): grow batches to the VRAM limit; target >= 32.
    # The Rust caller may also batch; this is the server-side ceiling/target.
    EMBED_MIN_BATCH: int = int(os.getenv("DIYRAG_EMBED_MIN_BATCH", "32"))
    EMBED_MAX_BATCH: int = int(os.getenv("DIYRAG_EMBED_MAX_BATCH", "256"))

    # Whether to actually load models at startup. Allows the contract/health
    # surface to be exercised in CI without GPUs (`/readyz` stays 503 until
    # real model load is wired — see TODOs).
    EAGER_LOAD: bool = os.getenv("DIYRAG_EAGER_LOAD", "false").lower() == "true"

    HOST: str = os.getenv("DIYRAG_GPU_HOST", "0.0.0.0")  # noqa: S104 (container-internal; gateway fronts it)
    PORT: int = int(os.getenv("DIYRAG_GPU_PORT", "8081"))


SETTINGS = Settings()


# ---------------------------------------------------------------------------
# Model registry — holds loaded handles. Populated at startup (lifespan).
# `ready` flips True only when every required model for the active profile is
# resident, gating /readyz (§0).
# ---------------------------------------------------------------------------


class ModelRegistry:
    def __init__(self) -> None:
        self.embedder: Any | None = None
        self.reranker: Any | None = None
        self.llm: Any | None = None
        self.ocr: Any | None = None
        self.ready: bool = False
        self.backend: str = SETTINGS.LLM_BACKEND
        self.device: str = SETTINGS.DEVICE
        self.loaded_at: float | None = None

    def load(self) -> None:
        """Load models per the active backend/profile.

        DECISION: model loading is deferred behind EAGER_LOAD so the HTTP
        contract and health surface are testable without a GPU. Real loading is
        a TODO to be wired in M2/M3 (§20).
        """
        # TODO(M2): load BGE-M3 dense+sparse embedder.
        #   - FlagEmbedding: `from FlagEmbedding import BGEM3FlagModel;
        #     self.embedder = BGEM3FlagModel(SETTINGS.EMBED_MODEL,
        #         use_fp16=(self.device == "cuda"), device=self.device)`
        #   - returns {"dense_vecs", "lexical_weights"} for dense+sparse.
        # TODO(M2): load reranker (FlagEmbedding FlagReranker or a CrossEncoder
        #   over SETTINGS.RERANK_MODEL).
        # TODO(M3): if self.backend == "vllm": construct `vllm.LLM(...)` /
        #   `AsyncLLMEngine`; else load a transformers `AutoModelForCausalLM`
        #   + tokenizer over SETTINGS.LLM_MODEL. Guard VRAM (§16 VRAM governance).
        # TODO(OCR): load Surya/Marker predictors for the /ocr contract.
        self.loaded_at = time.time()
        self.ready = True

    def shutdown(self) -> None:
        """Release device memory (§16: minimise host<->device churn on teardown)."""
        # TODO: free CUDA memory (torch.cuda.empty_cache(), del handles).
        self.embedder = self.reranker = self.llm = self.ocr = None
        self.ready = False


REGISTRY = ModelRegistry()


# ---------------------------------------------------------------------------
# Pydantic v2 request/response contracts (§3.2). These are the wire schema the
# Rust tier serialises against; keep field names stable.
# ---------------------------------------------------------------------------


class EmbedRequest(BaseModel):
    """Batch embed request. The Rust embed-client sends dynamically sized
    batches (target >= 32 — §6.5)."""

    texts: list[str] = Field(..., min_length=1, description="Inputs to embed.")
    normalize: bool = Field(default=True, description="L2-normalise dense vectors.")
    return_dense: bool = Field(default=True)
    return_sparse: bool = Field(default=True, description="BGE-M3 learned sparse (§5.2).")


class SparseVector(BaseModel):
    """Sparse (lexical) vector as parallel index/value arrays — matches Qdrant's
    sparse vector wire shape so the Rust side maps it straight through (§5.2)."""

    indices: list[int]
    values: list[float]


class EmbedItem(BaseModel):
    dense: list[float] | None = None
    sparse: SparseVector | None = None


class EmbedResponse(BaseModel):
    model: str
    dim: int | None = Field(default=None, description="Dense dimensionality.")
    embeddings: list[EmbedItem]


class RerankRequest(BaseModel):
    query: str = Field(..., min_length=1)
    documents: list[str] = Field(..., min_length=1)
    top_k: int | None = Field(default=None, ge=1, description="Keep top-k (§7.1).")


class RerankResult(BaseModel):
    index: int = Field(..., description="Index into the input `documents`.")
    score: float


class RerankResponse(BaseModel):
    model: str
    results: list[RerankResult]


class InferMessage(BaseModel):
    """Chat message. The Rust `retrieval` crate is responsible for separating
    trusted system instructions from untrusted retrieved content with explicit
    delimiters BEFORE calling this endpoint (§7.2, §12.5)."""

    role: str = Field(..., pattern="^(system|user|assistant)$")
    content: str


class InferRequest(BaseModel):
    messages: list[InferMessage] = Field(..., min_length=1)
    max_tokens: int = Field(default=1024, ge=1, le=32768)
    temperature: float = Field(default=0.2, ge=0.0, le=2.0)
    top_p: float = Field(default=0.95, ge=0.0, le=1.0)
    stop: list[str] | None = None


class InferResponse(BaseModel):
    model: str
    backend: str
    text: str
    prompt_tokens: int | None = None
    completion_tokens: int | None = None


class OcrRequest(BaseModel):
    """Image/region OCR. Image bytes are base64 in `image_b64`; the
    parsing-service owns whole-document scanned-PDF OCR — this is the
    finer-grained region contract (§16)."""

    image_b64: str = Field(..., min_length=1)
    languages: list[str] | None = None
    detect_tables: bool = Field(default=False)


class OcrBlock(BaseModel):
    text: str
    bbox: list[float] = Field(..., description="[x0, y0, x1, y1] page coords.")
    confidence: float | None = None


class OcrResponse(BaseModel):
    blocks: list[OcrBlock]
    full_text: str


class HealthResponse(BaseModel):
    status: str


class ReadyResponse(BaseModel):
    ready: bool
    backend: str
    device: str
    models_loaded_at: float | None = None


class Level(str, Enum):
    DEBUG = "DEBUG"
    INFO = "INFO"
    WARN = "WARN"
    ERROR = "ERROR"


# ---------------------------------------------------------------------------
# App + lifespan (model load/unload) + correlation-id middleware (§13.1).
# ---------------------------------------------------------------------------


@asynccontextmanager
async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
    configure_json_logging()
    LOG.info(
        "gpu-runtime starting",
        extra={"backend": SETTINGS.LLM_BACKEND, "device": SETTINGS.DEVICE},
    )
    if SETTINGS.EAGER_LOAD:
        REGISTRY.load()
        LOG.info("models loaded", extra={"loaded_at": REGISTRY.loaded_at})
    else:
        LOG.warning(
            "DIYRAG_EAGER_LOAD=false: models NOT loaded; /readyz will report 503 "
            "until model load is wired (see TODOs)."
        )
    try:
        yield
    finally:
        REGISTRY.shutdown()
        LOG.info("gpu-runtime stopped")


app = FastAPI(
    title="diyRAG gpu-runtime",
    version="0.1.0",
    summary="GPU inference sidecar: embeddings, reranker, LLM, OCR (§3.3, §16).",
    lifespan=lifespan,
)


@app.middleware("http")
async def correlation_id_middleware(request: Request, call_next: Any) -> Response:
    """Propagate the gateway-generated correlation id (§13.1). Generate one if
    absent so every log line and downstream hop is reconstructable."""
    correlation_id = request.headers.get(CORRELATION_HEADER) or str(uuid.uuid4())
    start = time.perf_counter()
    # Bind to the logging context for this request.
    logging.LoggerAdapter(LOG, {"correlation_id": correlation_id})
    try:
        response = await call_next(request)
    except Exception:  # noqa: BLE001 — convert to structured 500 below
        LOG.exception(
            "unhandled error", extra={"correlation_id": correlation_id, "path": request.url.path}
        )
        response = JSONResponse(
            status_code=500,
            content={"error": "internal_error", "correlation_id": correlation_id},
        )
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    response.headers[CORRELATION_HEADER] = correlation_id
    LOG.info(
        "request",
        extra={
            "correlation_id": correlation_id,
            "method": request.method,
            "path": request.url.path,
            "status": response.status_code,
            "elapsed_ms": round(elapsed_ms, 2),
        },
    )
    return response


# ---------------------------------------------------------------------------
# Health (§0, §11.2). /healthz = liveness (process up). /readyz = readiness
# (models resident); returns 503 until ready so orchestrators gate traffic.
# ---------------------------------------------------------------------------


@app.get("/healthz", response_model=HealthResponse, tags=["health"])
async def healthz() -> HealthResponse:
    return HealthResponse(status="ok")


@app.get("/readyz", response_model=ReadyResponse, tags=["health"])
async def readyz(response: Response) -> ReadyResponse:
    if not REGISTRY.ready:
        response.status_code = 503
    return ReadyResponse(
        ready=REGISTRY.ready,
        backend=REGISTRY.backend,
        device=REGISTRY.device,
        models_loaded_at=REGISTRY.loaded_at,
    )


# ---------------------------------------------------------------------------
# Inference endpoints. Each is a thin, deterministic adapter over a loaded
# model. The heavy lifting is left as explicit TODOs to be wired in M2/M3 (§20).
# ---------------------------------------------------------------------------


@app.post("/embed", response_model=EmbedResponse, tags=["inference"])
async def embed(req: EmbedRequest) -> EmbedResponse:
    """Produce BGE-M3 dense and/or sparse embeddings (§5.2, §6.5).

    Dynamic batching note (§6.5): the Rust embed-client sends batches sized to
    the VRAM ceiling (target >= 32). Server-side, group `req.texts` into chunks
    of [EMBED_MIN_BATCH, EMBED_MAX_BATCH] before the forward pass.
    """
    # TODO(M2): run BGE-M3 forward pass via REGISTRY.embedder.
    #   out = REGISTRY.embedder.encode(req.texts, batch_size=...,
    #       return_dense=req.return_dense, return_sparse=req.return_sparse)
    #   dense  -> out["dense_vecs"]      (np.ndarray [n, dim]; L2-normalise if req.normalize)
    #   sparse -> out["lexical_weights"] (list[dict[token_id -> weight]])
    #             -> map to SparseVector(indices=[...], values=[...])
    # Until wired, return an empty-but-valid envelope so the contract is exercisable.
    items = [EmbedItem(dense=None, sparse=None) for _ in req.texts]
    return EmbedResponse(model=SETTINGS.EMBED_MODEL, dim=None, embeddings=items)


@app.post("/rerank", response_model=RerankResponse, tags=["inference"])
async def rerank(req: RerankRequest) -> RerankResponse:
    """Cross-encoder rerank with bge-reranker-v2-m3 (§7.1)."""
    # TODO(M2): score pairs (req.query, doc) via REGISTRY.reranker.compute_score,
    #   sort descending, keep top_k. Preserve original `index` into `documents`.
    results = [RerankResult(index=i, score=0.0) for i in range(len(req.documents))]
    if req.top_k is not None:
        results = results[: req.top_k]
    return RerankResponse(model=SETTINGS.RERANK_MODEL, results=results)


@app.post("/infer", response_model=InferResponse, tags=["inference"])
async def infer(req: InferRequest) -> InferResponse:
    """Grounded generation. Backend selected by DIYRAG_LLM_BACKEND (§16, §24.1):
    "vllm" on Linux/CUDA throughput nodes, "transformers" otherwise."""
    # TODO(M3): branch on REGISTRY.backend.
    #   vllm:         engine.generate(prompt, SamplingParams(max_tokens=req.max_tokens,
    #                     temperature=req.temperature, top_p=req.top_p, stop=req.stop))
    #   transformers: apply chat template -> model.generate(...) -> decode.
    #   The Rust `retrieval` layer has already delimited trusted vs untrusted
    #   content in `messages`; do NOT re-interpret retrieved text (§12.5).
    return InferResponse(
        model=SETTINGS.LLM_MODEL or "<unset>",
        backend=REGISTRY.backend,
        text="",
        prompt_tokens=None,
        completion_tokens=None,
    )


@app.post("/ocr", response_model=OcrResponse, tags=["inference"])
async def ocr(req: OcrRequest) -> OcrResponse:
    """Image/region OCR via Surya/Marker (§16). Whole scanned-PDF OCR lives in
    the parsing-service; this is the finer-grained region contract."""
    # TODO(OCR): base64-decode req.image_b64 -> PIL.Image; run Surya detection +
    #   recognition (langs = req.languages or [SETTINGS.OCR_LANGS]); if
    #   req.detect_tables, run Marker/Surya table recognition. Map detected
    #   regions to OcrBlock(text, bbox=[x0,y0,x1,y1], confidence=...).
    return OcrResponse(blocks=[], full_text="")


if __name__ == "__main__":  # pragma: no cover — container entrypoint uses uvicorn directly
    import uvicorn

    uvicorn.run(
        "app.main:app",
        host=SETTINGS.HOST,
        port=SETTINGS.PORT,
        log_config=None,  # we own logging (JSON) via logging_config
    )
