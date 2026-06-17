# diyRAG

**Rust-first, self-hosted, multi-instance Retrieval-Augmented Generation.**

diyRAG ingests tens of thousands of heterogeneous documents, embeds them into a
vector store via parallel Rust workers, and serves hybrid semantic + keyword
retrieval and grounded, cited answers to many concurrent users across a LAN. It
runs as a **native Windows Service that auto-starts on reboot** and as a
**Docker Compose stack on unraid / Linux** — both driven from the same `diyrag`
terminal CLI.

> **Private repository.** diyRAG is intended to live in its own **private**
> GitHub repository named `diyRAG`. It is self-hosted on owned hardware
> (homelab / SMB); it is not a public SaaS. Keep it private and keep secrets
> out of git (see [`.env.example`](.env.example) and §12 of the spec).

---

## What it is

- **Ingestion** of `pdf, docx, doc, txt, md, rtf, html, epub, mobi, azw3, pptx,
  xlsx, csv, json, eml` (extensible plugin parsers), with watched folder roots,
  batch/archive processing, and **content retention** (removal is logical,
  audited, and reversible — never silent data loss).
- **Hybrid retrieval** (dense + sparse) with reranking, then **grounded answer
  generation with inline citations** and conflict flagging.
- **Multi-user, multi-tenant** with API-key + OAuth 2.1 auth and **per-tenant
  cryptographic isolation** (one Qdrant collection per tenant).
- **LAN multi-instance sync** of the processed-file registry and vector corpus
  over mTLS, surviving partitions (version-vector CRDT + Qdrant snapshots).
- **MCP server** (`rmcp`) so LLM clients can use the corpus as tools.
- **Observability**: structured `tracing` logs, OTLP traces, an append-only
  `error_log`, and a clickable reference code on every error.

## The Rust / Python split (and why)

The service tier is **Rust by default** — gateway, core API, retrieval,
ingestion orchestration + native parsing + chunking, sync, MCP, autoscaler,
the `diyragd` supervisor/Windows Service, and the `diyrag` CLI. Rust buys
single static binaries, tiny distroless images, no GC pauses (helps the p95
SLAs), and memory safety.

**Python exists in exactly two swappable sidecars**, behind stable gRPC/HTTP
interfaces, where its ML ecosystem is irreplaceable:

| Sidecar | Why Python | Endpoints |
|---|---|---|
| `gpu-runtime` | vLLM throughput + transformers for ONNX-less models + Surya/Marker OCR | `/embed` `/rerank` `/infer` `/ocr` |
| `parsing-service` | Docling layout/table + Surya/Marker for scanned/complex docs | gRPC hard-parse |

Rust-native equivalents (`ort`/`fastembed` for embeddings + rerank,
`mistral.rs` for the LLM, a native parser router) are the **default** and the
only GPU path on Windows. The common, well-formed document path involves **no
Python process at all**.

## Architecture (summary)

```
        Browser / Tauri client / MCP clients / diyrag CLI
                              │ HTTPS/WSS
                              ▼
                  api-gateway (Rust/Axum)        authN/Z, rate-limit, validate
                              │
                              ▼
                  core-api (Rust)                RAG + file mgmt orchestration
            ┌─────────────────┼─────────────────┐
            ▼                 ▼                 ▼
     NATS JetStream      retrieval          gpu-runtime (PYTHON)
     (async ingest)      (Rust hybrid       vLLM / BGE-M3 / reranker / OCR
            │             + rerank)               ▲
            ▼                                      │ hard parses
   ingestion-worker ×N (Rust) ────────────────────┘  parsing-service (PYTHON)
   parse→chunk→embed→persist
            │
   ┌────────┼─────────────┐
   ▼        ▼             ▼
 Postgres  Qdrant       Blob store          metadata / vectors / content
 (sqlx)    (per-tenant) (object_store)
            ▲
  sync-agent (Rust) ◀── tonic gRPC + mTLS ──▶ peer LAN node (mDNS discovery)

  Supervisor: diyragd  ── Windows Service (autostart) | systemd | Docker
  Control:    diyrag   ── service / node / ingest / query / snapshot
```

The full diagram and component table are in
[`ARCHITECTURE.md`](ARCHITECTURE.md) (mirrors spec §2).

---

## Quickstart A — Windows (install as an auto-starting service)

diyRAG runs as a **Windows Service** managed by the Service Control Manager, so
it **starts on every device reboot** and keeps ingesting even when no user is
logged in. State lives under `%ProgramData%\diyRAG` with restricted ACLs; logs
go to the Windows Event Log and rolling JSON files.

```powershell
# 1. Install via the MSI/winget package (drops diyragd.exe + diyrag.exe).
winget install diyRAG

# 2. First-run bootstrap: generate CA + service certs, pull models, migrate, seed.
diyrag bootstrap --generate-ca --pull-models

# 3. Install as an auto-starting service under a low-privilege account.
diyrag service install --mode all-in-one --auto-start --account "NT SERVICE\diyRAG"

# 4. Start it and confirm.
diyrag service start
diyrag service status

# Drive it headlessly from the terminal:
diyrag ingest "C:\Users\me\Documents" --watch
diyrag query "what changed in the Q2 report?" --answer
```

The native Tauri desktop client detects the local service and exposes
start/stop/restart (elevating via UAC). Raw SCM equivalents and the WinSW/NSSM
fallback are documented in `deploy/windows/`. See spec §16b.2.

## Quickstart B — unraid / Linux (Docker Compose)

The Compose stack is the **primary deployment path** on unraid/Linux.
`restart: unless-stopped` + unraid auto-starting Docker on array start returns
the stack after a reboot (the Linux analog of the Windows Service autostart).

```bash
# 1. Configure: copy the env template and fill in placeholders (NO real secrets in git).
cp .env.example .env
#   edit .env

# 2. Base stack (postgres, qdrant, minio, nats, redis, caddy + Rust services).
docker compose -f docker-compose.yml up -d

# 3. GPU node: add the NVIDIA overlay (needs the NVIDIA Container Toolkit /
#    on unraid the Nvidia Driver plugin). Use the vLLM profile for throughput.
docker compose -f docker-compose.yml -f docker-compose.gpu.yml --profile gpu up -d

# 4. First-run bootstrap + check.
just bootstrap      # or: make bootstrap
docker compose ps
docker compose logs -f core-api

# Headless control via the CLI (no GUI needed on unraid):
diyrag ingest /mnt/user/Documents --watch
diyrag query "summarize the network runbook" --answer
diyrag node snapshot
```

- **unraid Community Applications template:** `deploy/unraid/diyrag.xml` maps
  `appdata` volumes (`/mnt/user/appdata/diyrag` → Postgres/Qdrant/MinIO/
  model-cache/certs), WebUI port, env, and GPU passthrough. Point the unraid
  **CA template** at that file. (See spec §16b.3.)
- **Generic Linux:** `deploy/systemd/diyragd.service` runs
  `diyragd --mode all-in-one` natively (`Restart=always`).

### Task shortcuts (`just` or `make`)

```
up  down  dev  test  lint  seed  snapshot  restore  migrate
svc-install  svc-status  bootstrap
```

Run `just --list` or `make help`.

---

## Repository layout

```
/                         # repo root (the diyRAG repo)
├─ Cargo.toml             # [workspace] members = crates/*
├─ rust-toolchain.toml    # pinned toolchain
├─ docker-compose.yml     # base stack (Linux/unraid — PRIMARY path)
├─ docker-compose.gpu.yml # NVIDIA GPU overlay (vLLM profile)
├─ docker-compose.dev.yml # hot-reload + debug ports
├─ .env.example  justfile  Makefile  LICENSE
├─ README.md  ARCHITECTURE.md  MASTER_BUILD_SPEC.md  ADR/
├─ crates/                # Rust workspace members
│  ├─ common/             # config, logging, correlation-id, errors, auth, db, schemas, clients
│  ├─ api-gateway/        # edge: TLS, authN/Z, rate-limit, validate, WS/SSE, route
│  ├─ core-api/           # file mgmt, roots, batch + RAG orchestration
│  ├─ retrieval/          # hybrid search + rerank + context-condense
│  ├─ ingestion-worker/   # parser-router + chunker + embed-client + persistence
│  ├─ sync-agent/         # mDNS discovery, version-vector CRDT, snapshot replication
│  ├─ mcp-server/         # rmcp: tools/resources over Streamable HTTP + stdio
│  ├─ autoscaler/         # queue-depth → worker replica/task controller
│  ├─ diyragd/            # supervisor daemon + Windows Service + systemd/Docker entrypoint
│  └─ diyrag-cli/         # `diyrag` CLI: service / node / ingest / query / snapshot
├─ services-py/           # the ONLY Python
│  ├─ gpu-runtime/        # embeddings + reranker + LLM (vLLM) + OCR
│  └─ parsing-service/    # Docling + Surya/Marker hard parses (gRPC)
├─ web/                   # React/TS app (browser + Tauri bundle)
├─ native/               # Tauri 2 shell
├─ migrations/            # SQL (sqlx migrate)
├─ deploy/               # windows/ unraid/ systemd/
├─ infra/                # caddy, otel collector, grafana, CA scripts
├─ eval/                 # retrieval-quality harness (nDCG@10, recall@k, MRR)
└─ tests/                # integration / e2e / load / security
```

## Documentation

- **[MASTER_BUILD_SPEC.md](MASTER_BUILD_SPEC.md)** — the authoritative
  implementation contract. Treat it as the source of truth.
- **[ARCHITECTURE.md](ARCHITECTURE.md)** — diagram, component table, data flows,
  the Rust⇄Python boundary, and the dual-runtime deployment model.
- **[ADR/](ADR/)** — Architecture Decision Records (one per `DECISION:` note).

## Build phases

diyRAG is built in ordered phases **M0 → M10** (Bootstrap → Observability/
recovery/perf). Each phase has explicit exit criteria; do not start the next
phase until the current one's tests are green. See **spec §20** for the table.
This scaffolding satisfies the file-level deliverables of **M0 Bootstrap**.

## License

[MIT](LICENSE) © 2026 diyRAG contributors.
