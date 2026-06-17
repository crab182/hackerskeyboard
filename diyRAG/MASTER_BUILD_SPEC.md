# diyRAG — MASTER BUILD SPECIFICATION (Rust-first)
**Document type:** Implementation contract for AI pair-programmers (Codex / Cursor / Claude Code) and human engineers. Treat this file as authoritative.
**Version:** 2.0 — Rust-first re-plan of the original *Self-Hosted, Multi-Instance RAG Platform* spec (v1.0).
**Primary language:** **Rust** (services, workers, daemon, CLI, native shell, sync, MCP). **Python** is used **only** where it is genuinely best-in-class: deep-learning inference (vLLM / transformers) and document-AI parsing (Docling / Surya / Marker OCR).
**Target deployment:** Self-hosted on owned hardware (homelab/SMB), GPU-accelerated, multiple cooperating LAN instances. Two first-class runtimes:
1. **Windows-native** — runs as a **Windows Service** that auto-starts on device restart, managed from a terminal CLI.
2. **unraid / Linux** — runs as a **Docker Compose stack**, managed from the terminal (CLI + `docker compose`) with an unraid Community Applications template.

Kubernetes remains an OPTIONAL scale-out path, not the default.

---

## 0. HOW TO USE THIS PROMPT (read first, agent)

Build in the phases defined in §20, in order. After each phase:
1. Produce running, tested code that satisfies that phase's **exit criteria**.
2. Stop and emit: a short diff summary, the commands to run it, and the test results.
3. Do not begin the next phase until the current phase's tests pass (`cargo test` green for Rust crates; `pytest` green for the two Python services).

**Rules of engagement for the whole build:**
- Prefer **boring, durable, well-maintained** dependencies. When you pick a crate/library, justify it in one line against four factors: upfront cost, ongoing cost, skill/technical requirement, expected lifespan.
- **Rust by default.** Reach for Python only for the `gpu-runtime` and `parsing-service` (§3, §6, §16). If you are tempted to add a third Python service, write a `DECISION:` note explaining why a Rust crate cannot do the job.
- If a requirement is ambiguous or two valid designs exist, **state the assumption in a `DECISION:` comment in code and in your turn summary**, pick the more reversible option, and proceed. Do not block.
- **Security controls (authN, authZ, tenant isolation, input sanitization) are deterministic code OUTSIDE the LLM.** Never delegate an access-control decision to a model.
- Everything is configuration-driven via environment variables and a typed config crate (`common::config`). No hardcoded hosts, ports, secrets, or model names.
- Every service ships with: a `/healthz` (liveness) and `/readyz` (readiness) endpoint, structured JSON logging (`tracing`) with a propagated `correlation_id`, and a Dockerfile that runs as a non-root user.
- Write tests as you go (§18). A phase is "done" only when its tests are green.
- Generate `README.md`, `.env.example`, `ARCHITECTURE.md` (with the diagram from §2), and an `ADR/` folder capturing each `DECISION:`.

---

## 1. MISSION, SCOPE, NON-GOALS, ACCEPTANCE

**Mission.** A production-grade Retrieval-Augmented Generation platform that ingests tens of thousands of heterogeneous documents, embeds them into a vector store via parallel Rust workers, serves hybrid semantic+keyword retrieval and grounded answer generation to many concurrent users across multiple IPs, exposes itself through (a) a browser GUI (Chrome + Firefox), (b) a Windows-native desktop client **and a Windows Service**, (c) a Model Context Protocol (MCP) server for LLM clients, and (d) a terminal CLI for headless control on unraid/Linux — and synchronizes its processed-file registry and vector data across cooperating LAN instances over encrypted channels.

**In scope.**
- Ingestion of: `pdf, docx, doc, txt, md, rtf, html, epub, mobi, azw3, pptx, xlsx, csv, json, eml` (extensible plugin parsers; the named set is the minimum bar).
- Dynamic add/remove of files **and watched folder roots**, with **content retention** (removing a source never silently destroys ingested content/index; removal is logical + auditable).
- Batch processing (archives, large folder trees) with job tracking and progress.
- Parallel embedding/indexing (multiple Rust worker tasks/replicas, GPU-batched).
- Hybrid retrieval (dense + sparse) with reranking; grounded answer generation with citations.
- Multi-user, multi-tenant, API-key + OAuth2.1 auth; per-tenant cryptographic data isolation.
- LAN multi-instance sync of the document registry and vector payload.
- Error/log database, debugging UI, error recovery, observability.
- TLS 1.3 everywhere (`rustls`), mTLS for service-to-service, encryption at rest.
- **Run as a Windows Service (boot autostart) and as a Docker stack on unraid, both driven from a terminal CLI (§16b).**

**Non-goals (v1).** Public internet multi-tenant SaaS billing; mobile native apps; training/fine-tuning pipelines (leave hooks, don't build); Kubernetes production manifests (stubs only); cross-WAN federation (LAN-local sync only).

**Acceptance criteria (system is "done" when all hold):**
1. Ingest 25,000 mixed-format files end-to-end; ≥99% land in `INDEXED` or a classified `QUARANTINED` state with a reason; zero silent data loss.
2. p95 retrieval latency < 800 ms and p95 grounded-answer latency < 6 s under 50 concurrent query users on the reference hardware (§15).
3. Concurrent ingestion + querying do not deadlock or starve; querying stays responsive while a 10k-file batch runs.
4. A killed worker mid-batch loses no work units; the batch resumes and completes (idempotent reprocessing).
5. Two LAN instances converge to identical registry + retrievable corpus after a partition heals (eventual consistency, deterministic conflict resolution).
6. Tenant A can never retrieve Tenant B's chunks, even with a crafted adversarial embedding query (verified by a red-team test).
7. Removing a folder root logically purges it from all query results within one transaction, retains the data + audit trail, and is reversible.
8. Every error surfaced in any GUI carries a clickable reference code that deep-links to the matching `error_log` entry.
9. **The platform installs and runs as a Windows Service that survives a reboot and is controllable via `diyrag service …`; the same release runs on unraid via `docker compose up -d` and an unraid CA template, controllable via the same CLI.**

---

## 2. ARCHITECTURE OVERVIEW

Decoupled services communicating over a message broker (async ingestion path) and mTLS gRPC/HTTP (sync + inference paths). Single Compose stack on Linux/unraid; on Windows a single supervisor (`diyragd`) runs the Rust services in-process or as child processes. Workers scale by replica count (Linux) or task count (Windows).

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

**Data flow, ingestion:** root/file registered → `core-api` records `PENDING`, enqueues work units on NATS → `ingestion-worker` claims a unit (idempotent on content hash) → `ParserRouter` extracts text+structure (Rust-native parser, or delegates hard cases to the Python `parsing-service` over gRPC) → chunker → `gpu-runtime` batch-embeds → persist (chunk row in Postgres via `sqlx` + vector in tenant's Qdrant collection + original bytes in blob store) → `INDEXED`; failures → classified → retry/backoff or `QUARANTINED`.

**Data flow, query:** client → `api-gateway` (authN/Z, rate-limit) → `core-api` → `retrieval` (embed query → hybrid dense+sparse search scoped to tenant → rerank → optional context-condense pass) → `gpu-runtime` (generate grounded answer with citations) → response with `error_log`-linkable envelope.

---

## 3. TECHNOLOGY STACK & RATIONALE (Rust-first)

The single biggest change from v1.0: **the service tier is Rust, not Python.** Python survives only as two inference/parsing sidecars where its ML ecosystem is irreplaceable.

### 3.1 Why Rust for the service tier (the four-factor justification)
- **Upfront:** moderate (team ramp on ownership/async) — offset by `cargo` tooling, one toolchain, no virtualenv/runtime drift.
- **Ongoing:** low — single static binary per service, tiny `distroless`/`scratch` images, no GC pauses, predictable tail latency (helps the p95 SLAs in §1), memory-safety removes a whole class of CVEs.
- **Skill:** moderate but increasingly common; the borrow checker pays back in fewer prod incidents.
- **Lifespan:** long — Tokio/Axum/sqlx/tonic/rustls are mature, widely deployed, and actively maintained.

### 3.2 Stack table

| Layer | v1.0 (Python) | **diyRAG choice (Rust-first)** | One-line rationale | Alternative |
|---|---|---|---|---|
| Service framework | FastAPI | **Axum + Tower/Tower-HTTP** | async, mature, composable middleware; same stack for gateway/core/retrieval/mcp. | `actix-web` if you prefer actor model. |
| Async runtime | asyncio | **Tokio** | de-facto Rust async runtime; powers every crate below. | — |
| (De)serialization & validation | Pydantic v2 | **`serde` + `serde_json` + `garde`/`validator`** | typed structs = compile-time schema; runtime validation for inputs. | `schemars` to emit JSON Schema/OpenAPI. |
| Background workers | arq / Celery | **Native Tokio tasks consuming NATS JetStream** | no separate task framework; workers are just Rust binaries scaled by replica/task count. | `apalis` job crate if you want a job abstraction. |
| Message broker | NATS JetStream | **NATS JetStream via `async-nats`** | unchanged infra; first-class async Rust client, durable consumers, ack/nak/term. | RabbitMQ (`lapin`) or Kafka (`rdkafka`) at scale. |
| Vector DB | Qdrant | **Qdrant via `qdrant-client` (official Rust)** | unchanged infra; HNSW, payload filters, native hybrid, quantization, snapshots, per-collection multitenancy. | `pgvector` (one datastore) if corpus < ~1–5M chunks. |
| Relational/metadata | PostgreSQL 16 | **PostgreSQL 16 via `sqlx`** (async, compile-time-checked queries) | unchanged DB; `sqlx::query!` checks SQL against the schema at build time. | `sea-orm` if you want an ORM. |
| Migrations | alembic/sqitch | **`sqlx migrate` (SQL files)** + optional `refinery` | SQL-first, in-repo, reviewable. | — |
| Blob/object store | MinIO/FS | **`object_store` crate** (S3/MinIO/Azure/GCS/local FS) | one abstraction, swap backend by config; content-addressed retention. | `aws-sdk-s3` / `rust-s3` if S3-only. |
| Embeddings | BGE-M3 (PyTorch) | **Rust-native: `ort` (ONNX Runtime) / `fastembed`** for BGE-M3 dense+sparse | runs on Windows **and** Linux; CUDA/DirectML/CoreML via ORT execution providers; no Python on the hot path. | **Python `gpu-runtime`** (transformers) when a model has no ONNX export. |
| Reranker | bge-reranker-v2-m3 | **ONNX cross-encoder via `ort`** | in-process rerank; Rust. | Python fallback in `gpu-runtime`. |
| LLM inference | vLLM | **`mistral.rs`** (Rust, candle-based, OpenAI-compatible server) as default; **vLLM (Python)** for high-throughput Linux/CUDA | `mistral.rs`/`candle` run on Windows+Linux+Mac; vLLM stays the throughput king on Linux/CUDA and is selectable by profile. | `llama.cpp` server for low-VRAM/CPU/Apple. |
| Doc parsing (digital/clean) | Docling/PyMuPDF | **Rust-native router**: `lopdf`+`pdf-extract` (PDF text), `calamine` (xlsx/xls), `docx-rs`+`zip`+`quick-xml` (docx/pptx), `epub` (epub), `scraper`+`readability` (html), `csv`, `pulldown-cmark` (md), `mail-parser` (eml) | zero-Python path for the common, well-formed case. | — |
| Doc parsing (scanned/complex/layout) | Surya/Marker/Docling | **Python `parsing-service`**: Docling (layout+tables), Surya/Marker (GPU OCR) over gRPC | deep-learning document AI has no Rust peer; isolate it behind an interface. | `ocrs`/`tesseract` (Rust) for simple OCR. |
| Legacy/ebook conversion | LibreOffice/Calibre | **`soffice --headless` / `ebook-convert` spawned by Rust as a sandboxed child process** (`tokio::process` + caps) | external binaries; language-agnostic; no Python needed to drive them. | — |
| Native Windows client | Tauri 2 | **Tauri 2 (Rust shell, system WebView2)** wrapping the React app | aligns with Rust-first; small binary, sandboxed; talks to the local Windows Service. | Electron if deep Node/OS integration needed. |
| **Node daemon / Windows Service** | — (new) | **`diyragd` (Rust) using the `windows-service` crate** + **`diyrag` CLI (`clap`)** | one supervisor binary; SCM-managed auto-start on Windows, systemd/Docker entrypoint on Linux; CLI drives both (§16b). | NSSM/WinSW wrapper as fallback. |
| MCP server | FastMCP (Python) | **`rmcp` (official Rust MCP SDK)** — Streamable HTTP (stateless) + stdio | Rust-native, co-located with `core-api`, same RBAC. | FastMCP (Python) if you split it out. |
| File watching | watchdog | **`notify` crate** (cross-platform inotify/ReadDirectoryChangesW/FSEvents) | the Rust watchdog; debounced root watching. | — |
| Service-to-service RPC | gRPC (grpcio) | **`tonic` (gRPC over HTTP/2) + `rustls`** | idiomatic Rust gRPC; mTLS for sync + inference. | — |
| TLS / PKI | OpenSSL | **`rustls`** everywhere + **`rcgen`** for CA/cert issuance | memory-safe TLS 1.3, no OpenSSL footguns; programmatic short-lived certs. | `cfssl`/step-ca for larger fleets. |
| Auth | OAuth2.1/JWT | **`jsonwebtoken`, `oauth2`, `argon2`** (key hashing) | JWT verify, OAuth2.1 flows, salted-hash API keys. | — |
| Rate limiting | Redis buckets | **`tower_governor`** (in-proc) + **Redis (`fred`)** for distributed token buckets | per-IP and per-key buckets at the gateway. | — |
| CRDT registry | version vectors | **version vectors implemented in Rust** (opt. `automerge` crate) | deterministic conflict resolution keyed on `content_sha256`. | `automerge` if richer CRDT semantics needed. |
| LAN discovery | zeroconf | **`mdns-sd`** + static peer list | LAN peer discovery with admin cert-pinning. | — |
| Observability | OTel SDK (py) | **`tracing` + `tracing-subscriber` (JSON) + `tracing-opentelemetry` + `metrics`/`opentelemetry-otlp`** → Prometheus + Loki + Tempo + Grafana | structured logs, distributed traces, metrics from one ecosystem. | logs-in-Postgres only for minimal nodes. |
| Archives / bomb guard | zipfile | **`zip`, `tar`, `flate2`** + manual size/ratio/depth caps | safe decompression in the batch orchestrator. | — |
| MIME sniff | python-magic | **`infer`** (magic-byte typing) | true-type detection, not extension trust. | — |
| Hash / IDs | hashlib/uuid | **`sha2` (sha256), `uuid` (v7)** | content addressing + sortable PKs. | — |
| Tokenization | tiktoken | **`tokenizers` (HF Rust) / `tiktoken-rs`** | token-accurate chunk sizing. | — |
| GUI | React/TS/Vite | **React 18 + TypeScript + Vite + Tailwind + shadcn/ui** | unchanged; one bundle for browser + Tauri. | SvelteKit if preferred. |
| Reverse proxy / TLS term | Caddy/Traefik | **Caddy (auto-TLS)** in front, or `api-gateway` terminates TLS directly | mTLS, HTTP/2, routing. | Nginx/Traefik. |
| Secrets | Docker secrets + SOPS | **Docker secrets + SOPS-encrypted env**, optional Vault | no plaintext secrets on disk or in images. | Vault when you outgrow files. |

> **NOTE carried from v1.0:** logs stay in partitioned Postgres (ship to Loki optionally), Vault is optional, K8s is a documented future path. The HPA concept maps to **broker-queue-depth-driven worker autoscaling** implemented by a lightweight Rust `autoscaler` crate (§15).

### 3.3 The Rust ⇄ Python boundary (precise)
Python exists in exactly **two** deployable services, each behind a stable gRPC/HTTP interface so it is swappable:
1. **`gpu-runtime`** (Python) — owns GPU(s); endpoints `/embed`, `/rerank`, `/infer`, `/ocr`. Default models: BGE-M3 (dense+sparse), bge-reranker-v2-m3, the answer LLM via vLLM, Surya/Marker for OCR. **Rust-native equivalents exist** (`ort`/`fastembed` for embeddings+rerank, `mistral.rs` for the LLM) and are the default on Windows / CPU / low-VRAM nodes; vLLM/transformers is selected on Linux/CUDA throughput nodes by profile.
2. **`parsing-service`** (Python) — Docling + Surya/Marker for scanned/complex/layout-heavy documents. The Rust `ingestion-worker` calls it only when the Rust-native parser router decides a document is "hard" (low text density, OCR needed, complex tables). LibreOffice/Calibre conversions are spawned as child processes **by Rust**, not by Python.

Everything else — gateway, core API, retrieval, ingestion orchestration + native parsing + chunking, sync, MCP, autoscaler, daemon/service, CLI, native shell — is **Rust**.

---

## 4. REPOSITORY LAYOUT (Cargo workspace monorepo)

```
/                              # repo root (this is the diyRAG repo)
├─ Cargo.toml                  # [workspace] members = crates/*
├─ rust-toolchain.toml         # pinned toolchain
├─ docker-compose.yml          # base stack (Linux/unraid)
├─ docker-compose.gpu.yml      # GPU overlay (NVIDIA toolkit; vLLM profile)
├─ docker-compose.dev.yml      # hot-reload, exposed debug ports
├─ .env.example
├─ justfile / Makefile         # just up / test / lint / seed / snapshot / svc-install
├─ ARCHITECTURE.md  README.md  /ADR
├─ /crates                     # Rust workspace members
│   ├─ /common                 # lib: config, logging, correlation-id, errors, auth, db (sqlx), schemas (serde), qdrant/blob clients
│   ├─ /api-gateway            # edge: TLS(rustls), authN/Z, rate-limit, schema-validate, WS/SSE, route
│   ├─ /core-api               # file mgmt, roots, batch orchestration, RAG orchestration
│   ├─ /retrieval              # hybrid search + rerank + context-condense
│   ├─ /ingestion-worker       # parser-router (native) + parsing-service client, chunker, embed-client, persistence
│   ├─ /sync-agent             # peer discovery (mdns-sd), version-vector registry CRDT, Qdrant snapshot replication (tonic+mTLS)
│   ├─ /mcp-server             # rmcp: tools/resources over Streamable HTTP + stdio
│   ├─ /autoscaler             # broker-queue-depth → worker replica/task controller
│   ├─ /diyragd                # supervisor daemon + Windows Service (windows-service crate) + systemd/Docker entrypoint
│   └─ /diyrag-cli             # `diyrag` CLI (clap): service mgmt, node, ingest, query, snapshot
├─ /services-py                # the ONLY Python
│   ├─ /gpu-runtime            # embeddings + reranker + LLM (vLLM) + OCR; FastAPI/gRPC; CPU fallback
│   └─ /parsing-service        # Docling + Surya/Marker (hard-parse) over gRPC
├─ /web                        # React/TS app (served to browsers)
├─ /native                     # Tauri 2 shell wrapping /web build
├─ /migrations                 # SQL (sqlx migrate)
├─ /deploy
│   ├─ /windows                # service install/uninstall scripts, WinSW/NSSM fallback, GPU notes
│   ├─ /unraid                 # Community Apps template XML, docker run/compose commands, User Scripts
│   └─ /systemd                # diyragd.service unit for generic Linux
├─ /infra                      # caddy config, otel collector, grafana dashboards, CA scripts
├─ /eval                       # retrieval-quality harness (nDCG@10, recall@k, MRR) — Rust or Python
└─ /tests                      # integration / e2e / load / security (cross-crate)
```

---

## 5. DATA MODEL & SCHEMAS

Unchanged from v1.0 at the storage layer (Postgres + Qdrant + blob); only the access code changes (sqlx/qdrant-client/object_store instead of SQLAlchemy/qdrant-py/boto3). The schema is the integration contract for LAN sync, so it is intentionally identical so a Rust node and a (hypothetical) Python node could interoperate.

### 5.1 PostgreSQL (authoritative metadata)
All timestamps UTC; all PKs UUIDv7 (sortable, generated in Rust via `uuid`).

- `tenants(id, name, slug UNIQUE, created_at)` — isolation boundary.
- `users(id, tenant_id FK, email UNIQUE, display_name, status, created_at)`.
- `api_keys(id, tenant_id FK, user_id FK NULL, key_hash, prefix, scopes JSONB, domain_scope JSONB, expires_at, revoked_at NULL, last_used_at)` — store only an **argon2** salted hash; never the raw key. `scopes` = resource perms; `domain_scope` = which collections/roots the key may touch.
- `roles(id, name)` and `user_roles(user_id, role_id)` — RBAC (§12.6).
- `roots(id, tenant_id FK, path, description, is_active BOOL, watch BOOL, include_globs JSONB, exclude_globs JSONB, source_root_id, created_at)` — watched folder roots.
- `documents(id, tenant_id FK, root_id FK NULL, source_path, content_sha256, mime, bytes, parser, status ENUM, retention_status ENUM, version_vector JSONB, lang, page_count, error_ref NULL, blob_key, created_at, indexed_at, updated_at)`. `status ∈ {PENDING,PARSING,CHUNKING,EMBEDDING,INDEXED,QUARANTINED}`. `retention_status ∈ {ACTIVE,PURGED_LOGICAL}`. UNIQUE `(tenant_id, content_sha256)` enforces dedup/idempotency.
- `chunks(id, document_id FK, tenant_id FK, ordinal, text, token_count, section_heading, page_number, structure_type, embed_model, vector_id, created_at)`. `vector_id` mirrors the Qdrant point id. `structure_type ∈ {prose,table,heading,code,triple}`.
- `jobs(id, tenant_id FK, type ENUM{BATCH,REINDEX,SYNC}, status ENUM{PENDING,RUNNING,COMPLETE,FAILED,PARTIAL_FAILURE}, total_units, processed_count, failed_unit_count, threshold_pct, created_at, finished_at)`.
- `work_units(id, job_id FK, document_ref, content_sha256, state ENUM{QUEUED,IN_PROGRESS,SUCCESS,FAILURE,FAILED_RECOVERABLE,DLQ}, retry_count, last_error_ref, claimed_by, claimed_at)`.
- `error_log` (append-only; partition by month) — schema in §13.2.
- `audit_log(id, tenant_id, actor_user_id, actor_key_id, action, resource_type, resource_id, before JSONB, after JSONB, ip, correlation_id, at)`.
- `sync_state(record_key, tenant_id, version_vector JSONB, last_hash, updated_at, origin_node)` — LAN sync (§9).
- `nodes(id, name, priority INT, last_seen, cert_fingerprint, endpoint)` — known LAN peers.

Rust modeling: each table maps to a `#[derive(sqlx::FromRow, serde::Serialize, serde::Deserialize)]` struct in `common::schemas`; enums are `#[sqlx(type_name=…, rename_all="UPPERCASE")]`. JSONB columns map to `sqlx::types::Json<T>`.

### 5.2 Qdrant (vectors)
- **One collection per tenant** (hard isolation), named `t_{tenant_slug}`. Do **not** rely on payload-only filtering for isolation (§12.7 / §22).
- **Named vectors**: `dense` (BGE-M3 dense, cosine) and `sparse` (BGE-M3 learned sparse) for native hybrid query + fusion. Enable scalar/binary quantization; keep originals `on_disk`.
- Payload (filterable): `document_id, root_id, retention_status, lang, structure_type, page_number, ingestion_ts, source_sha256`. Payload indexes on `root_id`, `retention_status`, `document_id`.
- Point id = `chunks.vector_id` (UUID) → Postgres ↔ Qdrant joinable both directions.

### 5.3 Blob store (content retention)
Content-addressed key = `sha256/{first2}/{sha256}`, accessed via the `object_store` crate (S3/MinIO/local FS chosen by config). Stores original bytes so removing a source or unmounting a root never loses content; re-embed/repair always possible. GC only on explicit hard-delete with audit entry.

### 5.4 Canonical chunk record (in transit — serde-serialized, identical wire format to v1.0)
```json
{
  "chunk_id": "uuidv7",
  "tenant_id": "uuidv7",
  "document_id": "uuidv7",
  "text": "clean segment text",
  "metadata": {
    "source_file": "report.pdf",
    "content_sha256": "…",
    "root_id": "uuidv7",
    "page_number": 5,
    "section_heading": "Introduction",
    "structure_type": "prose",
    "ordinal": 12,
    "ingestion_ts": "2026-06-17T00:00:00Z",
    "version_vector": {"nodeA": 5, "nodeB": 2}
  }
}
```

---

## 6. INGESTION PIPELINE (Rust orchestration, selective Python)

### 6.1 Folder roots & file watching
- REST (see §11): register/deregister roots; trigger on-demand ingest of specific paths.
- A `watcher` task in `core-api` uses the **`notify`** crate on `watch=true` roots, honoring include/exclude globs (`globset` crate), debounced; new/changed files (by `content_sha256`, not mtime) are enqueued. Deleted source files set `retention_status=PURGED_LOGICAL` (logical, reversible) — never a hard delete by default.

### 6.2 Idempotent work units & parallelism
- Each file = one **idempotent** work unit keyed by `content_sha256`. Before processing, a worker checks `documents` for an existing `INDEXED` row with the same `(tenant_id, content_sha256)`; if present, it acks immediately.
- Workers are stateless Rust binaries, horizontally scalable (Linux replicas / Windows task count). They claim units atomically (NATS JetStream `ack`/`in-progress` + `work_units.state=IN_PROGRESS, claimed_by`). **At-least-once delivery (JetStream) + worker-level idempotency = effectively-once.**

### 6.3 Parser router (plugin architecture, Rust trait)
```rust
#[async_trait::async_trait]
pub trait Parser: Send + Sync {
    fn can_handle(&self, sniff: &MimeSniff) -> Confidence;
    async fn parse(&self, blob: BlobRef, opts: &ParseOpts) -> Result<StructuredDoc, ParseError>;
}
```
A `ParserRouter` selects a handler by MIME sniff (`infer`, magic bytes — never extension) → falls back by extension. Handlers return a normalized `StructuredDoc` (text blocks + headings + page/coords + tables-as-markdown). Required handlers:

| Type | Primary handler | Engine |
|---|---|---|
| PDF (digital) | `PdfTextParser` | **Rust**: `pdf-extract` / `lopdf` (fast text+structure). |
| PDF (scanned/complex) | `OcrParser` | **Python parsing-service**: Surya/Marker (GPU) — triggered when text density low or `force_ocr`. |
| DOCX/PPTX | `OoxmlParser` | **Rust**: `zip` + `quick-xml` (+ `docx-rs`); tables→markdown, heading hierarchy. Complex layout → Docling (Python) fallback. |
| XLSX/XLS/CSV | `SpreadsheetParser` | **Rust**: `calamine` (xlsx/xls), `csv` (csv) → row/section chunks. |
| DOC (legacy) | `LegacyOfficeParser` | **Rust spawns** `soffice --headless` → DOCX → `OoxmlParser` (sandboxed child). |
| HTML/MD/TXT/RTF | `MarkupParser` | **Rust**: `scraper`+readability (html), `pulldown-cmark` (md), `rtf-parser`/plaintext; **sanitize hidden text** (§12.5). |
| EPUB | `EpubParser` | **Rust**: `epub` crate (spine/HTML, reading order). |
| MOBI/AZW3 | `EbookConvertParser` | **Rust spawns** Calibre `ebook-convert` → EPUB → `EpubParser` (sandboxed child, resource caps). |
| JSON/EML | `StructuredParser` | **Rust**: `serde_json`, `mail-parser` (header+body+attachments, recurse). |

New formats = drop in a struct implementing `Parser` and register it — **no core changes**. Each handler is defensively coded: `tokio` timeouts, memory caps (cgroup/job-object), and "never trust the file" (§12.5, §22). The decision "is this PDF scanned?" is a cheap Rust heuristic (extracted-text density per page); only `hard` docs cross the Python boundary, keeping the common path Python-free.

### 6.4 Chunking (Rust)
- Default **structure-aware** chunking: split on paragraph/heading/table boundaries first, then pack to a target window using token counts from the **`tokenizers`** crate. Defaults: **~512 tokens, 64–96 token overlap** (configurable per collection). Keep tables intact; tag `structure_type`.
- Carry mandatory metadata (§5.4). Reject chunks that fail invariants (empty text, > hard max tokens, missing required metadata) to the quarantine path.

### 6.5 Embedding (parallel + GPU-batched)
- The embed step calls the embedding backend (`ort`/`fastembed` in-process **or** `gpu-runtime` `/embed`) with **dynamically sized batches** (grow to VRAM limit; target batch ≥ 32). Produce **both** dense and sparse vectors (BGE-M3). Persist atomically: chunk row (Postgres via `sqlx` transaction) + point (Qdrant) in a write retried as a unit, idempotent on `vector_id`.
- Quantization (INT8/scalar) at the store; optional model quantization for low-VRAM nodes (§16).

### 6.6 Retention & logical delete (the "keep contents" guarantee)
On `DELETE /roots/{id}`: set `roots.is_active=false`; async job sets `documents.retention_status=PURGED_LOGICAL` for that root and writes Qdrant payload `retention_status=PURGED_LOGICAL` on affected points. **Query layer always filters `retention_status=ACTIVE`.** Data + blobs remain; audit entry records the purge; a reverse endpoint reactivates. Hard physical delete is a separate, admin-only, audited operation.

### 6.7 Batch processing
- `POST /batch/submit` accepts archives (ZIP/TAR) or path lists; returns a `job_id` immediately + a status URL. Orchestrator decompresses in a sandbox using `zip`/`tar`/`flate2` with **size/compression-ratio/depth caps** (zip-bomb guard), checksums (`sha2`), fans out one idempotent work unit per file, sets `total_units`, publishes to NATS.
- `GET /batch/{job_id}/status` → `processed/total`, `failed_unit_count`, ETA. Job → `COMPLETE`, or `PARTIAL_FAILURE` if a configurable failure threshold (default 20%) is exceeded.

---

## 7. VECTOR STORE & RETRIEVAL (Rust)

### 7.1 Hybrid retrieval
- Query embedding (BGE-M3 dense+sparse) → Qdrant **hybrid search** (`qdrant-client` `query` API) with server-side fusion (RRF or weighted) over the tenant collection, filtered to `retention_status=ACTIVE` and any caller-supplied `root_id`/domain filters.
- Retrieve top-`k₀` (e.g., 40) → **rerank** with `bge-reranker-v2-m3` (ONNX via `ort`, or `gpu-runtime`) → keep top-`k` (8–12).

### 7.2 Answer generation (grounded)
- Optional **context-condense pass**: a cheap LLM call extracts only query-relevant facts from reranked chunks before final generation (prevents context-window collapse).
- Final generation (`mistral.rs` default / vLLM profile): system prompt **strictly separates trusted instructions from retrieved content** with explicit delimiters + trust markers (retrieved text labeled untrusted; §12.5/§22). Output must include **inline citations** mapping each claim → `document_id`+`page_number`, and must flag conflicting sources rather than averaging them.
- Return a structured envelope (`serde`) with answer, citations, retrieval scores, and an `error_log`-linkable `reference_code` on failure.

### 7.3 Reindex
- A `REINDEX` job re-embeds from blob/chunks when the embedding model or chunking config changes; zero-downtime swap (build new collection, atomically alias). `embed_model` stored per chunk so mixed-model corpora are detectable and reconcilable.

---

## 8. MCP SERVER (Model Context Protocol — the real one)

> Disambiguation carried from v1.0: the original plan's internal "MCP / Master Control Point" for LAN sync is the **sync-agent** (§9). This section is the **actual Model Context Protocol** so LLM clients (Claude Desktop, Cursor, OpenAI Agents, etc.) can use the RAG as tools.

- **SDK/transport:** the **official Rust MCP SDK (`rmcp`)**, co-located with `core-api`. Expose **Streamable HTTP** (stateless mode for horizontal scale) for remote clients **and** stdio for local. SSE transport is deprecated — do not use it. Target the current stable spec and design forward-compatible (stateless core, routing headers, `ttlMs`/`cacheScope` on list/resource results, OAuth/OIDC-aligned auth). Set CORS headers for browser MCP clients.
- **Auth:** OAuth 2.1 (or mTLS for service callers). The MCP server enforces the **same tenant scoping and RBAC** as the REST API — a thin protocol adapter over `core-api`, not a privilege bypass.
- **Tools (deterministic, least-privilege; high-risk ops require explicit confirmation flags):**
  - `rag.search(query, k, filters)` → reranked chunks + citations.
  - `rag.answer(query, k, filters)` → grounded answer + citations.
  - `documents.list(filters)`, `documents.get(id)`.
  - `documents.add(path|url)` / `roots.add(path)` — **gated** (scope `ingest`).
  - `roots.remove(id)` — **gated** (logical purge; scope `admin`).
  - `ingestion.status(job_id)`.
- **Resources:** read-only `document://{id}`, `chunk://{id}`, `collection://{tenant}` with `ttlMs`/`cacheScope`.
- **Tool-poisoning defense:** tool descriptions/metadata are static, reviewed Rust constants, **never templated from user/ingested content**.

---

## 9. LAN MULTI-INSTANCE SYNC (`sync-agent`, Rust)

Goal: cooperating instances converge on (a) the **processed-file registry** and (b) the **retrievable vector corpus**, over encrypted channels, surviving partitions.

- **Transport/security:** **`tonic` gRPC over HTTP/2 with mutual TLS (`rustls`)**; both peers present X.509 certs from a shared internal CA (§12.1). Peers enrolled in `nodes` with `priority` + cert fingerprint; unknown certs rejected.
- **Discovery:** **`mdns-sd`** on the LAN + static peer list fallback. New peers must be admin-approved (pin cert fingerprint) before sync — no auto-trust.
- **What syncs, and how:**
  - **Registry (documents/chunks metadata):** a CRDT-style log keyed by `content_sha256`. Each record carries a **version vector** (`{node: counter}`). Because identity is the content hash and writes are idempotent, most "conflicts" are no-ops. True concurrent metadata edits resolve by: (1) version-vector dominance if one causally dominates; else (2) deterministic tiebreak = highest `nodes.priority`, then lexicographically smallest `node_id`. **No wall-clock LWW** (clock skew). Implemented in Rust; `automerge` optional.
  - **Vector payload:** replicate via **Qdrant snapshots** (per-collection), pulled by peers and applied; deltas via the registry drive incremental snapshot/restore. Because vectors are deterministic given (blob + model + chunker config), a peer may alternatively **re-embed from synced blobs** if it runs a different embedding model — record `embed_model` to detect mismatch.
  - **Blobs:** content-addressed; peers fetch missing blobs by hash on demand (chunked HTTP over mTLS).
- **DoS protection:** token-bucket rate-limit on the sync endpoint (`governor`); accept **diffs against the last acknowledged manifest only**, never full re-uploads; bound per-sync record count.
- **Consistency model:** eventual consistency; a `SYNC` job reports convergence. After a healed partition, peers MUST reach identical `ACTIVE` corpora (acceptance #5).

---

## 10. GUI / UX SPECIFICATION

One React/TS codebase serves **browsers (Chrome + Firefox)** and is wrapped by **Tauri 2** for the **Windows-native client**. Presentation is fully decoupled from business logic via the REST/WS API (§11); adding a client type never touches backend services. The native client additionally talks to the **local Windows Service** for service status/start/stop affordances (§16b).

### 10.1 Aesthetic & system
- **Dark mode default**, light available; design tokens via Tailwind + shadcn/ui; clear hierarchy, depth/elevation, smooth transitions, skeleton/circular loaders. Responsive to tablet.
- **Accessibility: WCAG 2.1 AA minimum** — keyboard nav, visible focus rings, sufficient contrast (audit dark mode), correct ARIA roles, reduced-motion support.

### 10.2 Screens (minimum)
1. **Dashboard** — corpus size, ingestion throughput, queue depth, error rate, node/sync status, GPU utilization, **service/runtime status (Windows Service or container health)**.
2. **Search & Answer** — query box; search-only vs grounded-answer toggle; result cards with score/source/page; citation chips opening the source chunk; "show conflicts" panel.
3. **Library / Files** — browse documents/roots; status badges; add/remove roots and files; trigger reingest; per-doc detail with chunks + provenance.
4. **Batch / Jobs** — submit archives; live progress bars; per-unit drill-down; retry/requeue.
5. **Errors / Debug** — searchable, filterable `error_log` view; deep-linkable by `reference_code`; correlation-id trace view; quarantine queue with re-inject.
6. **Admin** — users, API keys, roles/RBAC matrix, peer/node management + cert pinning, model/config settings, snapshot/backup, **service control (install/start/stop on Windows)**. Privileged actions require step-up auth (2FA/confirm).
7. **Settings** — theme, chunking/retrieval params (per collection), help-bubble delay, language.

### 10.3 The contextual Help-Bubble system (explicit requirement)
- **Trigger:** hover over any interactive control, header, configurable field, **or technical term**; a secondary `?`-affordance for touch/keyboard (focus also triggers, for a11y).
- **Behavior:** floating help bubble after a **configurable hover delay (default 600 ms; up to 2 s)**; dismiss on blur/leave/Esc.
- **Content source:** help text from a **decoupled, versioned help-content store** (`help/*.json` keyed by `module.element.param`), NOT hardcoded. `<HelpAnchor id="…">` used everywhere; missing-key in dev = visible warning.
- **Glossary terms:** a `<Term>` wrapper auto-attaches a definition bubble for known jargon; definitions live in the same store.

### 10.4 Error visualization (explicit requirement)
- API errors use the standard envelope (§11.3). The GUI renders **two sections**: a plain-language **user explanation**, and a **diagnostic reference code** rendered as a **clickable element** navigating to `Errors/Debug` pre-filtered to that `error_log` entry. Non-admins never see raw stack traces; admins get technical detail + correlation trace.

### 10.5 Realtime
- Job progress, ingestion status, node/sync health stream over **WebSocket (WSS)** (Axum WS); reconnect with backoff; SSE/poll fallback only if WS unavailable.

---

## 11. API SURFACE

### 11.1 Conventions
- All client traffic flows through `api-gateway`. **HTTPS only**; JSON in/out; versioned base path `/api/v1`. Endpoint shape `POST /api/v1/{module}/{action}` or RESTful resource routes. OpenAPI generated from Rust types (`utoipa` + `aide`). Validate every payload against its schema (`garde`/`validator`) **before** routing.

### 11.2 Representative endpoints
| Action | Method | Path | Scope |
|---|---|---|---|
| Register root | POST | `/api/v1/files/roots` | ingest |
| Deactivate root (logical purge) | DELETE | `/api/v1/files/roots/{id}` | admin |
| Reactivate root | POST | `/api/v1/files/roots/{id}/reactivate` | admin |
| Trigger ingest | POST | `/api/v1/ingestion/trigger` | ingest |
| Batch submit | POST | `/api/v1/batch/submit` | ingest |
| Batch status | GET | `/api/v1/batch/{job_id}/status` | reader |
| Search | POST | `/api/v1/query/search` | reader |
| Grounded answer | POST | `/api/v1/query/answer` | reader |
| List/get documents | GET | `/api/v1/documents` , `/{id}` | reader |
| Error lookup | GET | `/api/v1/errors?ref={code}` | reader/admin |
| Keys CRUD | POST/DELETE | `/api/v1/admin/keys` | admin |
| Node/peer mgmt | POST/DELETE | `/api/v1/admin/nodes` | admin |
| Service/runtime status | GET | `/api/v1/admin/runtime` | admin |
| Health/Ready | GET | `/healthz`, `/readyz` | public |

### 11.3 Standard error envelope (all failures, serde-serialized)
```json
{
  "success": false,
  "error_id": "E403_USER_PERMS",
  "user_facing_message": "Access denied. You can't modify roles in this workspace.",
  "technical_details": "Missing scope write:role for ModuleID 7B9F.",
  "reference_code": "ERR-2026-… (matches error_log.log_id)",
  "correlation_id": "…",
  "timestamp": "…",
  "suggestion_link": "/app/errors?ref=ERR-2026-…"
}
```

---

## 12. SECURITY & HARDENING (highest-priority layer; wraps everything)

Zero-trust posture. Every control is mandatory and is **code outside the LLM**. Rust's memory safety removes buffer-overflow/UAF classes by construction, but does not remove logic flaws — the controls below still apply.

### 12.1 Transport encryption & PKI
- **TLS 1.3** (1.2 floor) everywhere via **`rustls`**; modern cipher suites only; SSL/TLS < 1.2 forbidden.
- **mTLS for all service-to-service (east-west)** traffic, the sync path, and the inference path; client + server present CA-signed X.509 certs (verified by `rustls` `ServerCertVerifier`/`ClientCertVerifier`).
- Internal **CA** via `rcgen` scripts + SOPS (small deployments) or step-ca/Vault (larger). **Cert lifespan ≤ 90 days; auto-rotate ≥ 7 days before expiry; failed rotation = high-sev alert + quarantine the identity.** Private keys never stored in images or app dirs.

### 12.2 API keys & service auth
- API keys: random ≥ 256-bit (`rand`), shown once, stored as **argon2** salted hash, carry **resource scopes AND domain scopes**. Gateway validates every key with **instant revocation** (negative cache + DB check).
- Service-to-service authN via **OAuth 2.0 client-credentials** or mTLS identity. Prefer short-lived tokens over static keys for internal calls.

### 12.3 Gateway controls
- **Rate limiting** per source IP and per API key/client id, configurable per endpoint, backed by Redis token buckets (`fred` + `governor`); stricter on ingestion, sync, answer endpoints.
- **Schema validation** at ingress; reject malformed payloads early. Request size caps; multipart limits; content-type allow-list (Tower middleware).

### 12.4 Input validation & injection defense
- Whitelist schemas (types, charset, length, ranges) on all structured inputs (`garde`).
- **Parameterized queries only** — `sqlx` parameter binding; never string-concatenate SQL. Context-aware output encoding to prevent XSS.
- File ingestion: **deep content inspection** — verify true type by magic bytes (`infer`), structural validation per format, size/ratio/depth bounds; reject zip bombs and malformed containers.

### 12.5 Untrusted-content handling (RAG-specific; OWASP LLM01/LLM08)
- **Treat every ingested document and every retrieved chunk as untrusted input.** Strip/normalize hidden-instruction vectors at ingest: zero-width characters, white-on-white / `font-size:0` text, HTML comments, off-screen CSS, control characters (`unicode-normalization` + sanitizer pass).
- At generation time, **separate trusted system instructions from retrieved content** with explicit delimiters and trust-level markers; instruct the model that retrieved text is data, not instructions. Optional **secondary "semantic firewall"** pass (small isolated model) inspects retrieved context/tool outputs before they enter the main context.
- **High-risk actions require human/explicit confirmation** (data export, external messaging, root removal, hard delete). Least-privilege tools, deny-by-default.

### 12.6 RBAC (least privilege)
Three baseline roles; permission checks in **Tower middleware before business logic**; every elevation/sensitive access writes an `audit_log` entry.

| Role | Capabilities | Prohibited |
|---|---|---|
| Reader | `read_vector`, `read_metadata`, `query` | ingest, config, role changes |
| Ingester | reader + `add_files`, `process_upload`, `write:ingest_logs` | role/user changes, system config, hard delete |
| Admin | all + `manage_users`, `manage_keys`, `manage_nodes`, `full_audit`, `modify_config`, `manage_service` | self-elevation without step-up 2FA |

### 12.7 Tenant & vector isolation
- **Hard isolation per tenant via separate Qdrant collections** (not app-layer payload filtering alone). Every retrieval is constructed server-side against the caller's collection; the tenant id is derived from the authenticated principal, **never** from client-supplied parameters.
- Guard against **embedding inversion / poisoned retrieval**: access-control the vector store, never expose raw vectors to clients, log/audit the knowledge base, authenticate ingestion sources, run RAG-poisoning red-team tests (§22).

### 12.8 Container & host hardening
- Non-root users; read-only root filesystem where possible; drop Linux capabilities; no `--privileged`; pinned base images; image scanning (Trivy) in CI; minimal images (`scratch`/`distroless` — trivial for static Rust binaries); secrets via Docker secrets, never `ENV` in Dockerfile; network segmentation (only `api-gateway` and `sync-agent` expose ports). **Windows:** the service runs under a dedicated low-privilege account, data under `%ProgramData%\diyRAG` with restricted ACLs, binaries Authenticode-signed (§16b).

### 12.9 Audit & testing
- Append-only `audit_log` for all privileged/sensitive actions. Adversarial testing in CI (prompt-injection suite; map to MITRE ATLAS / OWASP LLM Top-10). Dependency/model/dataset treated as supply-chain: `cargo-deny` (licenses + advisories) + `cargo-audit`, pinned `Cargo.lock`, pinned model hashes.

---

## 13. OBSERVABILITY

### 13.1 Logging & tracing
- Structured JSON logs via **`tracing` + `tracing-subscriber`**; **`correlation_id` generated at the gateway**, injected into `X-Correlation-ID` on all outbound HTTP and into NATS message headers, propagated through every hop (a `tracing` span field). **`tracing-opentelemetry`** exports traces (OTLP) to Tempo/Jaeger; metrics to Prometheus; logs to Loki; dashboards in Grafana.

### 13.2 Error/Debug database (`error_log`, append-only, monthly partitions)
| Field | Type | Notes |
|---|---|---|
| log_id | UUID | PK = the `reference_code` shown in UIs |
| timestamp | timestamptz | indexed |
| level | enum(DEBUG,INFO,WARN,ERROR,CRITICAL) | |
| service_name | varchar | originating service |
| user_id / api_key_id | varchar | nullable |
| correlation_id | UUID | mandatory; full request reconstruction |
| transaction_id | varchar | e.g., content hash / job id |
| message | text | |
| stack_trace | jsonb | nullable (Rust backtrace / error chain) |
| context | jsonb | request params (PII-scrubbed) |

### 13.3 Metrics (minimum)
Queue depth, ingestion rate, parse/embed/index latencies, retrieval p50/p95, answer p95, error rate by class, GPU utilization/VRAM/temperature, sync lag, per-tenant QPS, **service uptime/restart count**.

---

## 14. ERROR HANDLING & RECOVERY

- **Taxonomy:** `TRANSIENT` (network/timeout/GPU-OOM) → retry with **exponential backoff** (`T = 2^(n-1) × base`, via `backoff`/`tokio-retry`); `PERMANENT` (corrupt/unsupported) → straight to **quarantine** with reason. Errors modeled as a `thiserror` enum carrying a `Classification`.
- **Non-stop workers:** a failed unit is logged, counted (`failed_unit_count`), routed to quarantine/DLQ, and the worker **immediately continues** the next unit. No single bad file halts a batch. (Rust `Result` propagation makes the "log-and-continue" boundary explicit at the work-unit loop.)
- **Detector/heartbeat:** an orchestrator monitors job SLAs and worker acks; a unit with no ack within `timeout = max_proc + 2σ` flips `IN_PROGRESS → FAILED_RECOVERABLE`, increments retry, re-queues with a `recovery` header. After max retries (default 5) → **DLQ** and job → `PARTIAL_FAILURE`.
- **Idempotency:** before committing, a worker checks results exist for the `content_sha256`/work-unit id; if so, ack without rewriting.
- **Crash recovery:** unacked NATS messages redeliver; `IN_PROGRESS` units with stale `claimed_at` are reclaimed; persistence writes are transactional (`sqlx` tx) and replayable from blob.
- **GPU failsafe:** on CUDA OOM or thermal throttle, gracefully **downgrade** (vLLM→`mistral.rs`/`llama.cpp`/CPU; `ort` CUDA EP→CPU EP) and log `HW-THERMAL-LIMIT` / `HW-OOM` with the affected job; alert.
- **Quarantine UI + re-inject:** quarantined items visible with reason, re-injectable after root-cause fix.
- **Service-level recovery (Windows):** SCM recovery actions restart `diyragd` on crash; Linux uses `Restart=always` (systemd) or Docker `restart: unless-stopped` (§16b).

---

## 15. PERFORMANCE & SCALE

- **Reference hardware:** 1× modern NVIDIA GPU (≥16 GB VRAM) per inference node, 8+ CPU cores, 32–64 GB RAM, NVMe. GPU runtime + embeddings on the GPU node; Postgres/Qdrant/MinIO co-locate or split.
- **Scale targets:** ≥25k files ingested reliably; 50 concurrent query users at the §1 SLAs. Rust's lack of GC and low per-connection overhead make the p95 targets easier to hold under load.
- **Concurrency & backpressure:** bounded channels (`tokio::sync::mpsc` / semaphores); workers pull at a rate the GPU sustains; NATS queue depth drives a lightweight **`autoscaler`** crate that adjusts `ingestion-worker` replica count (Linux) or task count (Windows) between min/max. Separate NATS subjects/queues + priorities so interactive queries are never starved by bulk ingestion.
- **Throughput levers:** dynamic embedding batch sizing to VRAM; vector quantization; vLLM continuous batching / `mistral.rs` paged-attention + (optional) speculative decoding; cache query embeddings and frequent answers (in-proc `moka` cache + Redis), invalidated on reindex/purge.

---

## 16. GPU ACCELERATION LAYER

GPU is the **default** compute path for embeddings, reranking, OCR, and LLM inference; CPU is fallback only.

- **Two interchangeable inference backends behind one interface (`gpu-runtime` gRPC/HTTP contract):**
  - **Rust-native (default; cross-platform incl. Windows):** `ort` (ONNX Runtime; CUDA/TensorRT/DirectML/CoreML execution providers) for BGE-M3 embeddings + reranker; `mistral.rs`/`candle` for LLM generation. No Python.
  - **Python `gpu-runtime` (Linux/CUDA throughput profile):** vLLM (paged attention, continuous batching) for the answer model; transformers for any model lacking an ONNX export; Surya/Marker for OCR.
- **Containerization (Linux):** `docker-compose.gpu.yml` overlay uses the **NVIDIA Container Toolkit** (`deploy.resources.reservations.devices`); base image carries matching CUDA runtime; pin CUDA/cuDNN/torch (or ORT-CUDA) versions.
- **Windows GPU:** `ort` with the **CUDA** or **DirectML** execution provider; `mistral.rs`/`candle` with CUDA. (vLLM is not targeted on Windows — the Rust-native backend is the Windows GPU path.) Documented in `deploy/windows/GPU.md`.
- **Single point of GPU entry:** the inference backend owns the device(s), exposes `/embed`, `/rerank`, `/infer`, `/ocr`, and manages request queues + pre-allocated memory so a runaway job can't starve others.
- **VRAM governance:** monitor context vs. model VRAM limit; minimize host↔device copies; on OOM/thermal, fall back to CPU and emit §14 hardware codes.
- **Multi-GPU:** schedule by device (process isolation or sharding) and document the policy.

---

## 16b. NATIVE WINDOWS SERVICE & UNRAID DEPLOYMENT (new, explicit requirement)

> Requirement: *"the native windows app to run as a service upon device restart and via terminal commands to run on unraid."* This section is binding.

### 16b.1 The supervisor binary `diyragd` and the `diyrag` CLI
- **`diyragd`** (crate `diyragd`) is the single supervisor binary. Run modes (selected by config/flags):
  - `all-in-one` — single-node homelab mode: starts `core-api`, `retrieval`, `ingestion-worker`(s), `mcp-server`, `sync-agent`, and the Rust-native inference backend **as Tokio tasks in one process** (simplest for a Windows box), or as managed child processes when isolation is preferred.
  - `agent` — manages an external service set (e.g., orchestrates the Docker Compose stack on Linux/unraid) and reports health.
  - `service:<name>` — run exactly one service (used by Compose replicas).
- **`diyrag`** (crate `diyrag-cli`, built with `clap`) is the terminal control plane, identical on Windows and Linux:
  - `diyrag service install | uninstall | start | stop | restart | status` — manage the OS service (Windows SCM / systemd).
  - `diyrag node status | peers | snapshot | restore` — node + sync ops.
  - `diyrag ingest <path|root> [--watch]`, `diyrag batch submit <archive>`, `diyrag query "<q>" [--answer]` — drive the API headlessly (auth via API key/OAuth). Ideal for unraid where there is no desktop GUI.
  - `diyrag config show | set` — typed config (12-factor; env overrides).

### 16b.2 Windows Service (auto-start on device restart)
- Implemented with the **`windows-service`** crate (Rust bindings to the Service Control Manager). `diyragd` detects whether it was launched by the SCM (`service` subcommand / `StartServiceCtrlDispatcher`) vs. interactively.
- **Registration & autostart:** `diyrag service install` calls the SCM to create the service with:
  - `start_type = StartType::AutoStart` → **starts on every device restart** (boot).
  - `service_type = OwnProcess`.
  - dependencies declared (e.g., on Docker Desktop only if `agent` mode targets containers; in `all-in-one` mode no external deps).
  - a dedicated **low-privilege service account** (or a managed virtual account `NT SERVICE\diyRAG`); not LocalSystem unless GPU access requires it (documented trade-off).
  - **recovery actions**: restart on 1st/2nd/3rd failure with backoff (set via `sc.exe failure` or the crate's config) → satisfies §14 service-level recovery.
- **Lifecycle:** the service `service_main` registers a control handler for `Stop`/`Shutdown`/`Pause`/`Continue`, reports `Running`/`StopPending`/`Stopped` status, and on `Stop` cancels the Tokio runtime gracefully (drains in-flight work units, acks/naks NATS, flushes logs).
- **Data & logs:** state under `%ProgramData%\diyRAG\` (config, certs, model cache, sqlite-bootstrap), logs to the **Windows Event Log** (via `eventlog`/`tracing` layer) **and** rolling JSON files; restricted ACLs (Administrators + the service account only).
- **GPU under a service:** services run in session 0 with no desktop; the Rust-native `ort`/`mistral.rs` backend works headless. Document that CUDA/DirectML drivers must be installed machine-wide.
- **Packaging:** ship an **MSI/winget** package (WiX/`cargo-wix`) that drops `diyragd.exe` + `diyrag.exe`, registers the service, and writes default config. **Fallback** wrapper (**WinSW** or **NSSM**) documented for environments that prefer not to use the native SCM integration. Binaries are **Authenticode-signed**.
- **Native desktop client interaction:** the Tauri app detects the local service, shows its status, and exposes start/stop/restart (elevating via UAC), so a non-terminal user manages everything from the GUI; the service keeps running and ingesting even when the GUI is closed and across reboots.

Representative install (terminal, Windows):
```powershell
# via the CLI (preferred — wraps SCM)
diyrag service install --mode all-in-one --auto-start --account "NT SERVICE\diyRAG"
diyrag service start
diyrag service status

# raw SCM equivalent (fallback / reference)
sc.exe create diyRAG binPath= "C:\Program Files\diyRAG\diyragd.exe service --mode all-in-one" start= auto
sc.exe failure diyRAG reset= 86400 actions= restart/5000/restart/10000/restart/30000
sc.exe start diyRAG
```

### 16b.3 unraid / Linux deployment (terminal commands)
unraid is a Slackware-based NAS OS whose first-class app model is **Docker**. diyRAG targets it two ways:
- **Docker Compose (primary).** The full stack runs from the repo's compose files. On unraid, use the **Docker Compose Manager** plugin or the terminal:
  ```bash
  # base stack
  docker compose -f /mnt/user/appdata/diyrag/docker-compose.yml up -d
  # GPU node (NVIDIA, with the unraid Nvidia driver plugin)
  docker compose -f docker-compose.yml -f docker-compose.gpu.yml up -d
  docker compose ps
  docker compose logs -f core-api
  docker compose down
  ```
- **unraid Community Applications template.** `deploy/unraid/diyrag.xml` is a CA template (per-container, or a single "diyrag-stack" container that runs `diyragd --mode agent` and brings up the rest). Templates set: WebUI port (api-gateway), `appdata` volume mappings (`/mnt/user/appdata/diyrag` → Postgres/Qdrant/MinIO/model-cache/certs), env vars from `.env`, GPU passthrough (`--runtime=nvidia`, `NVIDIA_VISIBLE_DEVICES=all`), and `--restart unless-stopped` so it **comes back after an unraid reboot/array start**.
- **Boot autostart on unraid:** Docker's `restart: unless-stopped` + unraid auto-starting the Docker service on array start = the stack returns after a reboot (the Linux analog of the Windows Service autostart). For non-Docker control, a **User Scripts** plugin script (`deploy/unraid/userscript-start.sh`) running "At Startup of Array" can invoke `diyrag service start` / `docker compose up -d`.
- **Headless control:** everything is driven from the unraid terminal via the `diyrag` CLI (no GUI needed): `diyrag ingest /mnt/user/Documents --watch`, `diyrag query "…" --answer`, `diyrag node snapshot`.
- **Generic Linux (non-unraid):** `deploy/systemd/diyragd.service` (a systemd unit, `Restart=always`, `WantedBy=multi-user.target`) runs `diyragd --mode all-in-one` natively; `diyrag service install` wraps `systemctl enable --now`.

### 16b.4 Cross-platform service abstraction
`diyrag-cli` and `diyragd` share a `ServiceManager` trait with three impls — `WindowsScm` (`windows-service`), `Systemd` (`systemctl` wrapper), `DockerCompose` (compose wrapper) — selected at runtime by `cfg!(windows)` / config. This keeps the CLI surface identical across Windows, unraid, and generic Linux (acceptance #9).

---

## 17. DEPLOYMENT

- `docker-compose.yml` (base) + `.gpu.yml` (GPU) + `.dev.yml` (hot reload). Profiles: `cpu`, `gpu`, `rust-llm` (mistral.rs), `py-llm` (vLLM), `api-llm` (external). All services: healthchecks, restart policies, resource limits, non-root.
- `.env.example` documents every variable. **No secret committed**; Docker secrets / SOPS. `just up | down | test | lint | seed | snapshot | restore | svc-install`.
- **Windows:** MSI/winget install → Windows Service (§16b). **unraid/Linux:** Compose + CA template / systemd (§16b).
- **Backup/restore:** scheduled Postgres dumps + Qdrant snapshots + blob sync; documented restore runbook; snapshots are the unit of vector replication (§9).
- **First-run bootstrap:** create admin tenant/user, generate CA + service certs (`rcgen`), pull/cache models, run migrations (`sqlx migrate`), seed help-content store.

---

## 18. TESTING & QA

- **Unit (`cargo test`):** parsers (golden files per format incl. epub/mobi/scanned PDF), chunker invariants, auth/RBAC, conflict-resolution (version-vector) logic, idempotency. Python sidecars: `pytest` for parsing/inference adapters.
- **Integration:** ingest→index→retrieve→answer happy path per format; logical-purge filtering; batch with injected failures; crash-and-resume; **service install/start/stop on Windows + Linux** (CI matrix).
- **E2E:** browser (Playwright, Chrome + Firefox) and Tauri smoke; help-bubble appears with correct content; error code deep-links to `error_log`.
- **Load:** 25k-file ingestion; 50-user concurrent query; verify SLAs and no starvation.
- **Security:** prompt-injection / RAG-poisoning suite (hidden-text docs must not alter behavior); cross-tenant isolation test (acceptance #6); rate-limit and authZ tests; container scan + `cargo-audit`/`cargo-deny` in CI.
- **Retrieval eval harness (`/eval`):** labeled query→relevant-doc set; report **nDCG@10, recall@k, MRR**; gate merges that regress retrieval quality.

---

## 19. CODING STANDARDS & CONSTRAINTS

- **Rust:** `#![forbid(unsafe_code)]` in service crates (allow only in vetted FFI shims with justification); `clippy` clean (`-D warnings`); `rustfmt`; errors via `thiserror` (libs) / `anyhow` (bins) with classification; no `unwrap()`/`expect()` on runtime paths; tracing spans on every request; typed config; pinned `Cargo.lock`.
- **Python (sidecars only):** type hints + `mypy`; `ruff`/`black`; `pytest`; pinned `requirements.txt`/`uv.lock`.
- 12-factor config; structured logging; no `println!`/`print`. No secrets in code/logs. Every public item documented (`cargo doc`). CI runs `fmt`+`clippy`+`test`+`cargo-deny`+`cargo-audit`+image scan on every PR. Conventional commits. Each `DECISION:` recorded as an ADR. Modules behind traits (`Parser`, `EmbeddingBackend`, `LlmBackend`, `VectorStore`, `ServiceManager`) so each is swappable.

---

## 20. PHASED DELIVERY PLAN (build in this order)

| Phase | Deliverable | Exit criteria |
|---|---|---|
| **M0 Bootstrap** | Cargo workspace, `common` crate (config/logging/correlation/errors/auth stubs), Compose skeleton, Postgres + sqlx migrations, health endpoints, CI (`fmt`/`clippy`/`test`/`deny`). | `just up` runs; healthchecks green; CI passes. |
| **M1 Ingestion core** | Blob store (`object_store`), native parser router (PDF/DOCX/TXT/MD), chunker (`tokenizers`), single worker, Postgres persistence (no vectors). | One file → chunks in DB; idempotent on re-run. |
| **M2 Vectors + retrieval** | Inference backend (`ort`/`fastembed` BGE-M3), Qdrant per-tenant collection, hybrid search + reranker, eval harness. | Ingest → hybrid search returns relevant chunks; eval runs. |
| **M3 Answering** | Context-condense + `mistral.rs` grounded generation with citations + conflict flags. | `query/answer` returns cited answer; injection-test baseline. |
| **M4 Scale ingestion** | NATS, parallel workers, batch orchestration (bomb guard), retention/logical-delete, remaining parsers (epub/mobi/scanned→Python parsing-service/Office/eml). | 25k-file batch completes; non-stop on failures; logical purge works. |
| **M5 API + GUI** | api-gateway (authN/Z, rate-limit, validate), full REST/WS, React app (all screens), help-bubble, error viz, Tauri shell. | Browser + native smoke pass; help + error deep-links work. |
| **M6 Security hardening** | mTLS + CA (`rcgen`) + rotation, API keys + scopes + revocation, RBAC middleware, untrusted-content sanitization, tenant isolation tests, container hardening, `cargo-deny`/`audit`. | Acceptance #6 + security suite pass; rotation verified. |
| **M7 MCP server** | `rmcp` Streamable HTTP + stdio, tools/resources, OAuth2.1, same RBAC. | MCP Inspector + a real client drive search/answer. |
| **M8 LAN sync** | sync-agent: `mdns-sd` discovery + cert pinning, version-vector registry CRDT, Qdrant snapshot replication, blob fetch, rate-limited diffs. | Acceptance #5 (two nodes converge after partition). |
| **M9 Service + deploy** | `diyragd` supervisor, **Windows Service (auto-start) + `diyrag` CLI**, unraid CA template + Compose + User Script, systemd unit, MSI/winget package. | Acceptance #9: reboots into a running service on Windows; `docker compose up -d` + CA template run on unraid; CLI drives both. |
| **M10 Observability + recovery + perf** | OTel/Prometheus/Loki/Grafana, error_log UI, detector/heartbeat + DLQ + backoff, autoscaler, GPU failsafe, load tuning. | All §1 acceptance criteria met. |

> M9 is promoted to a first-class phase (it was implicit in v1.0) because the Windows-Service + unraid runtime is an explicit acceptance criterion (#9).

---

## 21. SELF-QA CHECKLIST (run before declaring done)

- [ ] Every named file type ingests via a golden-file test, including MOBI (Calibre path) and a scanned PDF (OCR/Python path).
- [ ] The common, well-formed document path ingests with **no Python process involved** (Rust-native parsers + Rust-native embeddings).
- [ ] No control-flow path lets a single bad file stop a batch.
- [ ] Tenant id is always server-derived from the authenticated principal; no endpoint trusts a client-supplied tenant/collection.
- [ ] Retrieval always filters `retention_status=ACTIVE`; purge is reversible + audited.
- [ ] Every error in every client carries a `reference_code` that opens the matching `error_log` row.
- [ ] No secret in any image, env file committed, or log line. mTLS verified east-west. Cert rotation tested.
- [ ] Help text is data-driven (store), not hardcoded; missing keys warn in dev.
- [ ] GPU OOM/thermal triggers fallback + the documented error code.
- [ ] Two LAN nodes converge to identical ACTIVE corpora after a simulated partition.
- [ ] Retrieval-quality eval does not regress vs. the recorded baseline.
- [ ] Idempotency: re-running an entire batch produces zero duplicate chunks/vectors.
- [ ] **Windows: the service installs, starts on a simulated reboot, drains gracefully on stop, and is fully controllable via `diyrag service …`.**
- [ ] **unraid/Linux: `docker compose up -d` and the CA template both bring up a healthy stack that returns after a reboot; the `diyrag` CLI controls it headlessly.**
- [ ] `#![forbid(unsafe_code)]` holds in all service crates (or each exception has an ADR); `clippy -D warnings` clean.

---

## 22. RED-TEAM REVIEW OF THIS SPEC (then hardened)

Carried forward from v1.0 with Rust-first and Windows/unraid additions.

| # | Attack / failure | Why it bites | Binding mitigation (implement) |
|---|---|---|---|
| 1 | **Indirect prompt injection via ingested docs** | LLMs can't separate instructions from data; ~5 poisoned docs can flip outputs. | §12.5: strip hidden text at ingest; delimit + trust-mark retrieved content; optional semantic-firewall; deny-by-default tools; human confirm for exfil-class actions. |
| 2 | **Cross-tenant retrieval via crafted embedding** | App-layer-only filtering leaks; embedding inversion recovers PII. | §12.7: separate Qdrant collection per tenant; server-derived tenant; never expose raw vectors; isolation red-team test gates release. |
| 3 | **API-key theft → broad access** | Static broad keys bypass user roles. | §12.2: scope keys by resource AND domain; short-lived service tokens; instant revocation; per-key rate limits; audit. |
| 4 | **LAN sync DoS / race** | Flood of tiny diffs saturates write path; LWW corrupts under clock skew. | §9: token-bucket on sync; diffs-vs-manifest only; bounded record counts; version-vector + deterministic tiebreak; cert-pinned peers only. |
| 5 | **MCP tool poisoning / over-privileged stdio** | Malicious tool metadata or a local stdio server with host perms. | §8: static reviewed tool descriptions; MCP enforces same RBAC; least-privilege; prefer Streamable HTTP + OAuth for remote. |
| 6 | **Zip-bomb / malicious file parsing** | Crafted PDF/zip exhausts CPU/RAM or hits parser bugs. | §12.4/§6.3: magic-byte typing, structural validation, size/ratio/depth caps, per-file timeouts + memory limits, sandboxed subprocess for LibreOffice/Calibre. Rust memory-safety blunts native-parser exploits. |
| 7 | **Worker starvation under bulk ingest** | A 25k batch hogs GPU; interactive queries stall. | §15: separate priority subjects/queues; rate-limited workers; autoscaler; query path isolated from ingest path. |
| 8 | **Context-window collapse / hallucinated merges** | Too many noisy chunks; conflicting sources averaged. | §7.2: rerank → context-condense; conflict-flagging; mandatory citations. |
| 9 | **Silent data loss on root removal** | Naive delete destroys index. | §6.6: logical purge + blob retention + audit + reactivate. |
| 10 | **Secret/PKI mismanagement** | Long-lived certs, secrets in images. | §12.1/§12.8: ≤90-day rotating certs (`rcgen`), vault/SOPS, no secrets in images, fail-rotation alerts. |
| 11 | **Embedding/model drift across nodes** | Peers on different embedding models return incompatible vectors. | §9/§7.3: record `embed_model`; detect mismatch; re-embed from synced blobs or replicate snapshots. |
| 12 | **System-prompt leakage** | Users extract instructions/secrets. | Assume system prompt is not secret; keep no secrets in it; authZ outside the model; OWASP LLM07 tests. |
| 13 | **Windows Service privilege escalation** | Service as LocalSystem + writable binary path = local privesc. | §16b/§12.8: dedicated low-priv service account, ACL-locked install dir, Authenticode-signed binaries, no unquoted service paths, least privilege for GPU. |
| 14 | **unraid container escape / GPU passthrough abuse** | `--privileged`/broad device mounts widen blast radius. | §12.8/§16b: no `--privileged`; scoped `NVIDIA_VISIBLE_DEVICES`; read-only rootfs where possible; appdata volumes with least privilege; only gateway/sync ports exposed. |

---

## 23. FEATURE BACKLOG (post-v1; build hooks now, implement later)

1. **Citation graph** — cross-document concept relationships, navigable graph.
2. **Semantic drift / scope-creep detection** — flag incoming docs diverging from the corpus distribution.
3. **Conflict-resolution layer** — surface contradictory claims explicitly.
4. **Query-intent modeling** — route informational vs comparative vs procedural queries.
5. **Multi-step agent workflows** — decompose complex queries into sequential retrieval steps.
6. **Explanatory justification chain** — per-sentence source→evidence breakdown.
7. **Out-of-vocabulary fallback** — gated, audited external lookup.
8. **Readability scoring / optional simplification** of answers.
9. **Retrieval hyperparameter feedback loop** — track which `k`/fusion weights yield best satisfaction.
10. **Per-collection fine-tuned embeddings (LoRA)** — eval harness as the gate.
11. **Single-binary Windows "appliance" installer** that bundles Postgres/Qdrant/MinIO (embedded modes) for a one-click homelab node.

---

## 24. OPEN DECISIONS FOR THE HUMAN (sane defaults assumed meanwhile)

1. **Inference backend default:** assumed **Rust-native (`ort` + `mistral.rs`)** so the same release runs as a Windows Service and on unraid; **vLLM (Python)** auto-selected on Linux/CUDA throughput nodes. Confirm, or pin one backend everywhere.
2. **Single-tenant vs multi-tenant:** assumed **multi-tenant with per-tenant Qdrant collections** (you cited different IPs/users/instances). If this is one private corpus for you alone, it collapses to one collection and simplifies sync.
3. **"Sync the vectorized database":** assumed **Qdrant snapshot replication + content-addressed blob fetch + version-vector registry**. Confirm full corpus replication on every node (vs. a designated index node others query).
4. **Broker:** assumed **NATS JetStream** (`async-nats`). Say if you prefer RabbitMQ or Kafka.
5. **Windows runtime shape:** assumed **`all-in-one` `diyragd` as a Windows Service** for a single box, with the Tauri app as the GUI. If you want each service in its own container on Windows (Docker Desktop / WSL2), `diyragd --mode agent` orchestrates Compose instead — say which.
6. **Python sidecar reach:** assumed Python is confined to `gpu-runtime` (when vLLM/OCR profile is on) and `parsing-service` (hard parses). If you want a **zero-Python** deployment, accept the trade-off: Rust-native OCR (`ocrs`/tesseract) and ONNX-only models, with reduced quality on scanned/complex documents.

---

*End of master build specification. Hand to the agent one phase at a time (§20). Do not paste this whole file as a single coding prompt; feed §0–§5 plus the active phase's sections, and keep §22 (red-team) and §18 (tests) in context for every phase.*
