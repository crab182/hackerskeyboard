"""StructuredDoc contract for the parsing-service (MASTER_BUILD_SPEC.md §6.3).

This is the normalized document the Rust `ParserRouter` expects back from a hard
parse: text blocks + headings + page/coords + tables-as-markdown. Keep field
names stable — they are the wire contract shared with the Rust side (the gRPC
`.proto` in `proto/parsing.proto` mirrors these exactly).
"""

from __future__ import annotations

from enum import Enum

from pydantic import BaseModel, Field


class StructureType(str, Enum):
    """Mirror of Postgres `chunks.structure_type` (§5.1)."""

    PROSE = "prose"
    TABLE = "table"
    HEADING = "heading"
    CODE = "code"
    TRIPLE = "triple"


class BBox(BaseModel):
    """Page coordinates for a block: [x0, y0, x1, y1] in PDF points (top-left
    origin). Carried through to retrieval so citations can deep-link to a region
    (§7.2)."""

    x0: float
    y0: float
    x1: float
    y1: float


class TextBlock(BaseModel):
    """A normalized unit of document content. Tables are emitted as markdown in
    `text` with `structure_type == TABLE` and kept intact (§6.4 keeps tables
    whole)."""

    text: str
    structure_type: StructureType = StructureType.PROSE
    page_number: int | None = Field(default=None, ge=1)
    section_heading: str | None = None
    bbox: BBox | None = None
    # Heading depth (1 = top-level) when structure_type == HEADING.
    heading_level: int | None = Field(default=None, ge=1, le=9)
    ordinal: int = Field(..., ge=0, description="Reading-order index within the document.")
    confidence: float | None = Field(
        default=None, ge=0.0, le=1.0, description="OCR/layout confidence when applicable."
    )


class ParseHints(BaseModel):
    """Hints the Rust router passes alongside the blob (§6.3)."""

    mime: str | None = None
    force_ocr: bool = Field(default=False, description="Skip the text-density heuristic; OCR.")
    languages: list[str] | None = None
    # Defensive caps (§12.5 / §12.4). Server clamps to its own hard ceilings.
    max_pages: int | None = Field(default=None, ge=1)
    timeout_secs: int | None = Field(default=None, ge=1)


class ParseHardRequest(BaseModel):
    """`parse_hard(blob_ref, hints)` (§6.3). The Rust worker passes a content-
    addressed `blob_ref` (sha256 key §5.3); this service fetches the bytes from
    the shared blob store. For small/inline cases `inline_bytes_b64` may carry
    the payload directly."""

    blob_ref: str | None = Field(default=None, description="Content-addressed blob key (sha256).")
    inline_bytes_b64: str | None = Field(default=None, description="Inline payload (small docs).")
    content_sha256: str | None = Field(default=None, description="For idempotency/verification.")
    hints: ParseHints = Field(default_factory=ParseHints)


class StructuredDoc(BaseModel):
    """Normalized parse result returned to the Rust router (§6.3)."""

    blocks: list[TextBlock]
    page_count: int | None = Field(default=None, ge=0)
    lang: str | None = None
    parser: str = Field(..., description="Engine used, e.g. 'docling', 'surya', 'marker'.")
    ocr_used: bool = False
    content_sha256: str | None = None


class ParseError(BaseModel):
    """Structured failure. `classification` maps to the Rust §14 taxonomy so the
    worker decides retry-vs-quarantine."""

    classification: str = Field(..., description="'TRANSIENT' or 'PERMANENT' (§14).")
    code: str = Field(..., description="e.g. PARSE-TIMEOUT, PARSE-OOM, PARSE-UNSUPPORTED.")
    message: str


class HealthResponse(BaseModel):
    status: str


class ReadyResponse(BaseModel):
    ready: bool
    docling_loaded: bool
    ocr_loaded: bool
