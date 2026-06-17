"""diyRAG parsing-service — hard-parse sidecar (MASTER_BUILD_SPEC.md §3.3, §6.3, §12.5).

The Rust `ingestion-worker`'s `ParserRouter` handles the common, well-formed case
in Rust with ZERO Python (§6.3, §21). It calls this service ONLY when its cheap
text-density heuristic decides a document is "hard": scanned PDFs (OCR needed),
complex/layout-heavy tables, etc. This service then runs Docling (layout+tables)
and Surya/Marker (GPU OCR) and returns the normalized `StructuredDoc` (§6.3).

LibreOffice / Calibre conversions are spawned as sandboxed child processes BY THE
RUST WORKER, not here (§3.2, §6.3).

DECISION (§0/§24.6): the contract is exposed over **HTTP/JSON (FastAPI)** for
ergonomics and testability, with a gRPC `.proto` (`proto/parsing.proto`) mirroring
the same schema so a node may switch to `tonic`<->grpcio without changing the
wire shape. The spec allows "gRPC or FastAPI" (§6.3); HTTP is the more reversible
default and shares the health/observability surface with `gpu-runtime`.

Defensive posture (§12.4, §12.5, §22 #6 — hostile input):
  - per-request wall-clock timeout (clamped to a server ceiling),
  - page/byte caps to bound memory (zip-bomb / decompression-bomb guard),
  - "never trust the file": all extracted text is data, hidden-instruction
    stripping is the Rust side's job at ingest, but we never execute content,
  - structured ParseError with §14 classification so the worker retries vs
    quarantines deterministically.
"""

from __future__ import annotations

import asyncio
import base64
import logging
import os
import time
import uuid
from contextlib import asynccontextmanager
from typing import Any, AsyncIterator

from fastapi import FastAPI, Request, Response
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from .schemas import (
    HealthResponse,
    ParseError,
    ParseHardRequest,
    ReadyResponse,
    StructuredDoc,
)

CORRELATION_HEADER = "X-Correlation-ID"
LOG = logging.getLogger("diyrag.parsing_service")


class Settings:
    """12-factor config (§19). NO secrets, NO hardcoded hosts."""

    HOST: str = os.getenv("DIYRAG_PARSING_HOST", "0.0.0.0")  # noqa: S104 — container-internal
    PORT: int = int(os.getenv("DIYRAG_PARSING_PORT", "8082"))

    # Hard server-side ceilings — requests cannot exceed these (§12.4 caps).
    MAX_PAGES_CEILING: int = int(os.getenv("DIYRAG_PARSE_MAX_PAGES", "2000"))
    MAX_BYTES_CEILING: int = int(os.getenv("DIYRAG_PARSE_MAX_BYTES", str(512 * 1024 * 1024)))
    TIMEOUT_SECS_CEILING: int = int(os.getenv("DIYRAG_PARSE_TIMEOUT_SECS", "600"))
    DEFAULT_TIMEOUT_SECS: int = int(os.getenv("DIYRAG_PARSE_DEFAULT_TIMEOUT_SECS", "120"))

    DEVICE: str = os.getenv("DIYRAG_DEVICE", "cuda").lower()
    OCR_LANGS: str = os.getenv("DIYRAG_OCR_LANGS", "en")
    EAGER_LOAD: bool = os.getenv("DIYRAG_EAGER_LOAD", "false").lower() == "true"

    # Blob store access (§5.3) — object_store-compatible base; concrete fetch is
    # a TODO. NO credentials are read here; they come from the mounted secret.
    BLOB_BASE_URL: str = os.getenv("DIYRAG_BLOB_BASE_URL", "")


SETTINGS = Settings()


class Engines:
    """Holds loaded Docling / OCR predictors; gates /readyz."""

    def __init__(self) -> None:
        self.docling: Any | None = None
        self.ocr: Any | None = None
        self.ready: bool = False

    def load(self) -> None:
        # TODO(M4): construct Docling DocumentConverter (layout + table structure).
        #   from docling.document_converter import DocumentConverter
        #   self.docling = DocumentConverter()
        # TODO(M4): construct Surya/Marker predictors on SETTINGS.DEVICE for OCR.
        self.ready = True

    def shutdown(self) -> None:
        self.docling = self.ocr = None
        self.ready = False


ENGINES = Engines()


def _clamp_caps(req: ParseHardRequest) -> tuple[int, int]:
    """Resolve effective (max_pages, timeout_secs) under server ceilings (§12.4)."""
    max_pages = min(req.hints.max_pages or SETTINGS.MAX_PAGES_CEILING, SETTINGS.MAX_PAGES_CEILING)
    timeout = min(
        req.hints.timeout_secs or SETTINGS.DEFAULT_TIMEOUT_SECS, SETTINGS.TIMEOUT_SECS_CEILING
    )
    return max_pages, timeout


async def _load_blob(req: ParseHardRequest) -> bytes:
    """Fetch the document bytes, enforcing the byte ceiling (§12.4)."""
    if req.inline_bytes_b64:
        data = base64.b64decode(req.inline_bytes_b64, validate=True)
        if len(data) > SETTINGS.MAX_BYTES_CEILING:
            raise _PermanentParseError("PARSE-TOO-LARGE", "inline payload exceeds byte ceiling")
        return data
    if req.blob_ref:
        # TODO(M4): fetch content-addressed blob via the shared object_store
        #   (§5.3) at SETTINGS.BLOB_BASE_URL; stream with a hard byte cap; verify
        #   sha256 against req.content_sha256 (idempotency / tamper check).
        raise _TransientParseError("PARSE-BLOB-FETCH", "blob fetch not yet wired (TODO M4)")
    raise _PermanentParseError("PARSE-NO-INPUT", "neither blob_ref nor inline_bytes_b64 supplied")


class _ClassifiedError(Exception):
    classification = "PERMANENT"

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message

    def to_model(self) -> ParseError:
        return ParseError(classification=self.classification, code=self.code, message=self.message)


class _PermanentParseError(_ClassifiedError):
    classification = "PERMANENT"


class _TransientParseError(_ClassifiedError):
    classification = "TRANSIENT"


async def _do_parse(req: ParseHardRequest) -> StructuredDoc:
    """Run the hard parse. Wrapped in a timeout by the caller."""
    _ = await _load_blob(req)
    max_pages, _timeout = _clamp_caps(req)
    # TODO(M4): branch on hints/heuristic:
    #   - scanned / hints.force_ocr -> Surya/Marker OCR over the rendered pages,
    #     producing TextBlock(structure_type=PROSE/TABLE, bbox=..., page_number,
    #     confidence). Respect `max_pages`.
    #   - complex layout / tables -> Docling convert; map Docling's body items to
    #     TextBlock; render tables to markdown (structure_type=TABLE, kept intact
    #     §6.4); preserve heading hierarchy (structure_type=HEADING, heading_level)
    #     and reading order via `ordinal`.
    #   Set ocr_used, page_count, lang, parser accordingly.
    return StructuredDoc(
        blocks=[],
        page_count=0,
        lang=req.hints.languages[0] if req.hints.languages else None,
        parser="pending",
        ocr_used=bool(req.hints.force_ocr),
        content_sha256=req.content_sha256,
    )


@asynccontextmanager
async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
    _configure_json_logging()
    LOG.info("parsing-service starting", extra={"device": SETTINGS.DEVICE})
    if SETTINGS.EAGER_LOAD:
        ENGINES.load()
    else:
        LOG.warning("DIYRAG_EAGER_LOAD=false: engines not loaded; /readyz will report 503.")
    try:
        yield
    finally:
        ENGINES.shutdown()
        LOG.info("parsing-service stopped")


app = FastAPI(
    title="diyRAG parsing-service",
    version="0.1.0",
    summary="Hard-parse sidecar: Docling + Surya/Marker for scanned/complex docs (§6.3).",
    lifespan=lifespan,
)


@app.middleware("http")
async def correlation_id_middleware(request: Request, call_next: Any) -> Response:
    correlation_id = request.headers.get(CORRELATION_HEADER) or str(uuid.uuid4())
    start = time.perf_counter()
    try:
        response = await call_next(request)
    except Exception:  # noqa: BLE001
        LOG.exception("unhandled error", extra={"correlation_id": correlation_id})
        response = JSONResponse(
            status_code=500, content={"error": "internal_error", "correlation_id": correlation_id}
        )
    response.headers[CORRELATION_HEADER] = correlation_id
    LOG.info(
        "request",
        extra={
            "correlation_id": correlation_id,
            "method": request.method,
            "path": request.url.path,
            "status": response.status_code,
            "elapsed_ms": round((time.perf_counter() - start) * 1000.0, 2),
        },
    )
    return response


@app.get("/healthz", response_model=HealthResponse, tags=["health"])
async def healthz() -> HealthResponse:
    return HealthResponse(status="ok")


@app.get("/readyz", response_model=ReadyResponse, tags=["health"])
async def readyz(response: Response) -> ReadyResponse:
    if not ENGINES.ready:
        response.status_code = 503
    return ReadyResponse(
        ready=ENGINES.ready,
        docling_loaded=ENGINES.docling is not None,
        ocr_loaded=ENGINES.ocr is not None,
    )


class ParseHardResponse(BaseModel):
    """Either `doc` (success) or `error` (classified failure) is set."""

    doc: StructuredDoc | None = None
    error: ParseError | None = None


@app.post("/parse_hard", response_model=ParseHardResponse, tags=["parse"])
async def parse_hard(req: ParseHardRequest, response: Response) -> ParseHardResponse:
    """`parse_hard(blob_ref, hints)` -> StructuredDoc (§6.3).

    Defensive: enforces a wall-clock timeout and returns a classified ParseError
    instead of a 500 so the Rust worker can decide retry (TRANSIENT) vs
    quarantine (PERMANENT) per §14, and continue the batch (§14 non-stop).
    """
    _max_pages, timeout = _clamp_caps(req)
    try:
        doc = await asyncio.wait_for(_do_parse(req), timeout=timeout)
        return ParseHardResponse(doc=doc)
    except asyncio.TimeoutError:
        response.status_code = 422
        return ParseHardResponse(
            error=ParseError(
                classification="TRANSIENT",
                code="PARSE-TIMEOUT",
                message=f"parse exceeded {timeout}s wall-clock cap",
            )
        )
    except _ClassifiedError as exc:
        response.status_code = 422
        return ParseHardResponse(error=exc.to_model())
    except MemoryError:
        response.status_code = 422
        return ParseHardResponse(
            error=ParseError(
                classification="TRANSIENT", code="PARSE-OOM", message="parser exhausted memory"
            )
        )


def _configure_json_logging(level: int = logging.INFO) -> None:
    """Match the gpu-runtime JSON log shape (§13.1)."""
    try:
        from pythonjsonlogger import jsonlogger
    except Exception:  # pragma: no cover — dev without the dep
        logging.basicConfig(level=level)
        return
    import sys

    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(
        jsonlogger.JsonFormatter(
            "%(asctime)s %(levelname)s %(name)s %(message)s",
            rename_fields={"asctime": "timestamp", "levelname": "level"},
        )
    )
    root = logging.getLogger()
    root.handlers.clear()
    root.addHandler(handler)
    root.setLevel(level)


if __name__ == "__main__":  # pragma: no cover
    import uvicorn

    uvicorn.run("app.main:app", host=SETTINGS.HOST, port=SETTINGS.PORT, log_config=None)
