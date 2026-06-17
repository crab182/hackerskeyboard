"""Contract/health smoke tests for parsing-service (MASTER_BUILD_SPEC.md §18).

Run without GPU/models (DIYRAG_EAGER_LOAD defaults false): exercise the HTTP
surface, the StructuredDoc schema, and the defensive caps — not real parsing.
"""

from __future__ import annotations

import base64
import os

os.environ.setdefault("DIYRAG_EAGER_LOAD", "false")

from fastapi.testclient import TestClient  # noqa: E402

from app.main import app  # noqa: E402


def test_healthz_live() -> None:
    with TestClient(app) as client:
        assert client.get("/healthz").status_code == 200


def test_readyz_503_until_engines_loaded() -> None:
    with TestClient(app) as client:
        resp = client.get("/readyz")
        assert resp.status_code == 503
        assert resp.json()["ready"] is False


def test_parse_hard_no_input_is_permanent_error() -> None:
    with TestClient(app) as client:
        resp = client.post("/parse_hard", json={"hints": {}})
        assert resp.status_code == 422
        err = resp.json()["error"]
        assert err["classification"] == "PERMANENT"
        assert err["code"] == "PARSE-NO-INPUT"


def test_parse_hard_blob_ref_transient_until_wired() -> None:
    with TestClient(app) as client:
        resp = client.post("/parse_hard", json={"blob_ref": "sha256/ab/abc", "hints": {}})
        assert resp.status_code == 422
        # Blob fetch is a TODO (M4) -> TRANSIENT so the worker retries, not quarantines.
        assert resp.json()["error"]["classification"] == "TRANSIENT"


def test_parse_hard_inline_ok_shape() -> None:
    payload = base64.b64encode(b"hello").decode()
    with TestClient(app) as client:
        resp = client.post("/parse_hard", json={"inline_bytes_b64": payload, "hints": {}})
        assert resp.status_code == 200
        assert resp.json()["doc"] is not None
