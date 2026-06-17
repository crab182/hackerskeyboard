# diyRAG — Architecture

This document is the navigable companion to [`MASTER_BUILD_SPEC.md`](./MASTER_BUILD_SPEC.md). Section references (§) point into that spec.

diyRAG is a **Rust-first**, self-hosted, multi-instance Retrieval-Augmented Generation platform. The service tier, workers, sync, MCP server, supervisor daemon, CLI, and native shell are all Rust. **Python is confined to two inference/parsing sidecars** where its ML ecosystem is irreplaceable (§3.3, [ADR-0002](./ADR/0002-python-confined-to-inference-and-hard-parsing.md)).

It ships two first-class runtimes:
- **Windows** — a single supervisor binary (`diyragd`) installed as a **Windows Service that auto-starts on every reboot**, controlled from a terminal via the `diyrag` CLI (§16b, [ADR-0003](./ADR/0003-windows-service-and-unraid-runtime.md)).
- **unraid / Linux** — a **Docker Compose stack** (+ an unraid Community Apps template), controlled from the terminal via `docker compose` and the same `diyrag` CLI.

---

## 1. System diagram (§2)

```
                         ┌────────────────────────────────────────────────┐
        Browser (Chrome/FF) ─┐                                              │
        Tauri native client ─┼──HTTPS/WSS──▶  api-gateway (Rust/Axum) ──┐   │
        MCP clients ─────────┤  (REST + WS + Streamable HTTP)           │   │
        diyrag CLI (term) ───┘  authN/authZ, rate-limit, validate, route│   │
                                      │                                  │   │
                                      ▼                                  │   │
                       ┌──────────────┴───────────────┐  reads/writes    │
                       │       core-api (Rust)         │◀────────────────┤
                       │ (RAG orchestration, file mgmt)│                  │
                       └───┬───────────┬───────────┬───┘                  │
            publish jobs   │           │ query     │ embed/infer/ocr      │
                           ▼           ▼           ▼                       │
                   NATS JetStream   retrieval    gpu-runtime  ◀── PYTHON   │
                   (async-nats)     (Rust:       (vLLM + BGE-M3 + reranker │
                           │         hybrid+      + Surya/Marker OCR;      │
                           │         rerank)      CPU/Rust fallback)       │
              ┌────────────┼─────────────┐            ▲                    │
              ▼            ▼             ▼            │ (hard parses)      │
        ingestion-     ingestion-    ingestion-       │                    │
         worker 1       worker 2      worker N  ──────┘  parsing-service   │
        (Rust: parse→chunk→embed-client→persist)         ◀── PYTHON        │
              └──────┬──────┴──────┬──────┘             (Docling/Surya/    │
       writes chunks │             │ writes vectors      Marker/Calibre)   │
                     ▼             ▼                                       │
          ┌──────────────┐   ┌──────────────┐   ┌──────────────┐          │
          │  Postgres 16 │   │   Qdrant     │   │  Blob store  │          │
          │ (metadata,   │   │ (vectors,    │   │ (object_store│          │
          │  registry,   │   │  per-tenant  │   │  S3/MinIO/FS,│          │
          │  jobs, errors│   │  collections)│   │  content-    │          │
          │  RBAC, audit)│   └──────────────┘   │  addressed)  │          │
          │  via sqlx    │                      └──────────────┘          │
          └──────────────┘                                                │
                     ▲                                                    │
        sync-agent ──┘  ◀── tonic gRPC + mTLS (rustls) ──▶  sync-agent (peer LAN node)
        (Rust: registry version-vector CRDT + Qdrant snapshot replication, mDNS discovery)

   Supervisor:  diyragd (Rust) ── Windows Service (auto-start on boot) | systemd | Docker entrypoint
   Control:     diyrag (Rust CLI) ── service install/start/stop, node, ingest, query, snapshot
```

---

## 2. Component map

| Component | Language | Crate / package | Responsibility | Spec |
|---|---|---|---|---|
| `api-gateway` | Rust | `crates/api-gateway` | TLS edge; authN/Z; per-IP & per-key rate limit (governor+Redis); schema validation; CORS; correlation-id; WS/SSE; routes to core-api. | §11, §12.3 |
| `core-api` | Rust | `crates/core-api` | File/root management (`notify` watcher); batch orchestration (zip-bomb guard); RAG orchestration; retention/logical-delete. | §6, §7 |
| `retrieval` | Rust | `crates/retrieval` | Qdrant hybrid (dense+sparse) search + RRF fusion + `ACTIVE` filter; cross-encoder rerank; context-condense. | §7 |
| `ingestion-worker` | Rust | `crates/ingestion-worker` | NATS consumer; `ParserRouter` (native parsers + Python hard-parse delegation); chunker; embed client; atomic persist. | §6 |
| `gpu-runtime` | **Python** | `services-py/gpu-runtime` | GPU `/embed` `/rerank` `/infer` `/ocr` (vLLM, BGE-M3, Surya/Marker). Optional — Rust `ort`/`mistral.rs` is the default backend. | §16 |
| `parsing-service` | **Python** | `services-py/parsing-service` | Hard document parses only (Docling, Surya/Marker OCR) via gRPC. | §6.3 |
| `sync-agent` | Rust | `crates/sync-agent` | mDNS discovery + cert pinning; version-vector CRDT registry; Qdrant snapshot replication; mTLS gRPC. | §9 |
| `mcp-server` | Rust | `crates/mcp-server` | Model Context Protocol (`rmcp`): Streamable HTTP + stdio; tools/resources; same RBAC as REST. | §8 |
| `autoscaler` | Rust | `crates/autoscaler` | Scales ingestion-worker by NATS queue depth; keeps query subjects isolated. | §15 |
| `diyragd` | Rust | `crates/diyragd` | Supervisor daemon; **Windows Service** (auto-start) / systemd / Docker entrypoint; graceful drain. | §16b |
| `diyrag` (CLI) | Rust | `crates/diyrag-cli` | Terminal control plane: service mgmt, node ops, ingest, query, config. `ServiceManager` = WindowsScm/Systemd/DockerCompose. | §16b |
| `common` | Rust | `crates/common` | Shared lib: config, logging, correlation-id, errors, auth, db (sqlx), schemas (serde), vector & blob clients, ids. | §5 |
| Web GUI | TS/React | `web/` | Browser app + Tauri bundle; all screens, help-bubble, error viz, WS realtime. | §10 |
| Native client | Rust/Tauri | `native/` | Tauri 2 shell wrapping `web/`; manages the local Windows Service. | §10 |

**Datastores:** PostgreSQL 16 (authoritative metadata, `sqlx`), Qdrant (per-tenant vector collections), blob store (`object_store`: S3/MinIO/FS, content-addressed). NATS JetStream is the async ingestion broker; Redis backs distributed rate-limit buckets.

---

## 3. Data flow — ingestion (§2, §6)

root/file registered → `core-api` records `PENDING`, enqueues idempotent work units (keyed by `content_sha256`) on NATS → `ingestion-worker` claims a unit → `ParserRouter` extracts text+structure with a **Rust-native parser**, delegating only *hard* documents (scanned PDFs, complex layout) to the Python `parsing-service` over gRPC → chunker (structure-aware, ~512 tok) → embed (`ort`/`fastembed` in-proc, or `gpu-runtime`) producing dense+sparse vectors → atomic persist (chunk row in Postgres + point in the tenant's Qdrant collection + original bytes in the blob store) → `INDEXED`. Failures are classified `TRANSIENT` (retry w/ backoff) or `PERMANENT` (quarantine); a bad file never halts the batch (§14).

## 4. Data flow — query (§2, §7)

client → `api-gateway` (authN/Z, rate-limit) → `core-api` → `retrieval`: embed query → Qdrant hybrid dense+sparse search **scoped to the tenant's collection** and filtered to `retention_status=ACTIVE` → RRF fusion → cross-encoder rerank (top-40→top-8..12) → optional context-condense → `gpu-runtime`/`mistral.rs` grounded generation with **inline citations** and conflict flags → response envelope carrying an `error_log`-linkable `reference_code`.

---

## 5. The Rust ⇄ Python boundary (§3.3, ADR-0002)

Python lives in exactly two deployable services, each behind a stable gRPC/HTTP interface so it is swappable:

1. **`gpu-runtime`** — embeddings/rerank/LLM/OCR. The **default** backend is Rust-native (`ort`/`fastembed` + `mistral.rs`/`candle`), which runs on Windows and Linux; the Python vLLM/transformers backend is selected on Linux/CUDA throughput nodes by profile.
2. **`parsing-service`** — Docling + Surya/Marker for scanned/complex documents only. The Rust worker's native router handles every clean/digital format and spawns LibreOffice/Calibre as sandboxed children itself.

Everything else is Rust. The common, well-formed ingestion path involves **no Python process at all**.

---

## 6. Dual-runtime deployment model (§16b, §17, ADR-0003)

| | Windows | unraid | Generic Linux |
|---|---|---|---|
| Shape | `diyragd` `--mode all-in-one` as a **Windows Service** | Docker Compose stack + CA template, or `diyragd --mode agent` | Compose, or `diyragd` under systemd |
| Boot autostart | SCM `StartType::AutoStart` | Docker `restart: unless-stopped` + array auto-start (+ User Script) | systemd `WantedBy=multi-user.target`, `Restart=always` |
| Control | `diyrag service …` (wraps SCM) / Tauri GUI | `diyrag` CLI + `docker compose` | `diyrag service …` (wraps systemctl) |
| GPU | `ort` CUDA/DirectML, `mistral.rs` CUDA (no vLLM) | NVIDIA Container Toolkit; vLLM or Rust-native | NVIDIA toolkit |
| Recovery | SCM failure actions (restart) | Docker restart policy | systemd `Restart=always` |

A single `ServiceManager` trait (`WindowsScm` / `Systemd` / `DockerCompose`) gives the CLI an identical surface across all three (acceptance #9).

---

## 7. Cross-cutting concerns

- **Security (§12):** TLS 1.3 (`rustls`) everywhere; mTLS east-west; per-tenant Qdrant collections ([ADR-0006](./ADR/0006-per-tenant-qdrant-collections.md)); argon2-hashed scoped API keys; RBAC in Tower middleware; untrusted-content sanitization; deny-by-default tools; append-only audit log. Rust memory safety removes a class of native CVEs ([ADR-0001](./ADR/0001-rust-first-service-tier.md)).
- **Observability (§13):** `tracing` JSON logs with a gateway-minted `correlation_id` propagated through HTTP headers and NATS message headers; OTLP traces → Tempo; metrics → Prometheus; logs → Loki; Grafana dashboards.
- **Sync (§9, ADR-0005):** version-vector CRDT with deterministic tiebreak (priority → node-id), **never wall-clock LWW**; Qdrant snapshot replication; content-addressed blob fetch.
- **Recovery (§14):** idempotent work units, heartbeat/DLQ, exponential backoff, GPU OOM/thermal CPU fallback, OS-level service restart.

See `MASTER_BUILD_SPEC.md` §20 for the phased build order and §18/§21 for the test and self-QA gates.
