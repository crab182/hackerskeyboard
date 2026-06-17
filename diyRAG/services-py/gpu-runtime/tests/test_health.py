"""Contract/health smoke tests for gpu-runtime (MASTER_BUILD_SPEC.md §18).

These run without a GPU (DIYRAG_EAGER_LOAD defaults to false): they exercise the
HTTP surface and the Pydantic v2 schemas, not real inference.
"""

from __future__ import annotations

import os

os.environ.setdefault("DIYRAG_EAGER_LOAD", "false")

from fastapi.testclient import TestClient  # noqa: E402

from app.main import app  # noqa: E402


def test_healthz_is_live() -> None:
    with TestClient(app) as client:
        resp = client.get("/healthz")
        assert resp.status_code == 200
        assert resp.json()["status"] == "ok"


def test_readyz_503_until_models_loaded() -> None:
    # With EAGER_LOAD=false, models are not resident -> readiness must be 503.
    with TestClient(app) as client:
        resp = client.get("/readyz")
        assert resp.status_code == 503
        assert resp.json()["ready"] is False


def test_embed_contract_shape() -> None:
    with TestClient(app) as client:
        resp = client.post("/embed", json={"texts": ["hello", "world"]})
        assert resp.status_code == 200
        body = resp.json()
        assert body["model"]
        assert len(body["embeddings"]) == 2


def test_rerank_top_k_truncates() -> None:
    with TestClient(app) as client:
        resp = client.post(
            "/rerank",
            json={"query": "q", "documents": ["a", "b", "c"], "top_k": 2},
        )
        assert resp.status_code == 200
        assert len(resp.json()["results"]) == 2


def test_infer_rejects_bad_role() -> None:
    with TestClient(app) as client:
        resp = client.post("/infer", json={"messages": [{"role": "bogus", "content": "x"}]})
        assert resp.status_code == 422  # pydantic validation


def test_correlation_id_echoed() -> None:
    with TestClient(app) as client:
        resp = client.get("/healthz", headers={"X-Correlation-ID": "test-corr-1"})
        assert resp.headers.get("X-Correlation-ID") == "test-corr-1"
