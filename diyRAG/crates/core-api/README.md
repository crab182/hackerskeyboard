# diyrag-core-api

The orchestration tier of diyRAG (binary `diyrag-core-api`). It sits behind
`api-gateway` (which owns TLS termination, authN/Z, and rate limiting at the
edge) and owns the authoritative metadata in Postgres. See
`MASTER_BUILD_SPEC.md` §2, §6, §7, §11.

## Responsibilities

- **File & root management** (`files.rs`, spec §6.1): register/deregister watched
  folder roots; a `notify`-based watcher honors per-root include/exclude globs
  (`globset`), debounces events, and enqueues new/changed files **by
  `content_sha256`, not mtime**. Removed source files are logically purged, never
  hard-deleted.
- **Retention & logical delete** (`roots.rs`, spec §6.6): `DELETE
  /files/roots/{id}` sets `retention_status = PURGED_LOGICAL` for the root's
  documents (reversible, audited); data + blobs are retained; the query layer
  always filters `retention_status = ACTIVE`.
- **Batch orchestration** (`batch.rs`, spec §6.7): `POST /batch/submit` accepts
  archives/path lists, decompresses behind a zip/tar **bomb guard**
  (size / ratio / depth / entry-count caps), fans out one idempotent work unit
  per file, and publishes to NATS JetStream. `GET /batch/{job_id}/status`
  reports progress + failure counts.
- **RAG orchestration** (`rag.rs`, spec §7): `query/search` and `query/answer`
  call the `retrieval` service (hybrid search → rerank → optional condense) and
  the `gpu-runtime` service (grounded generation with mandatory citations and
  conflict flagging).

## Boundaries

- Tenant id is always derived server-side from the authenticated principal
  forwarded by the gateway — never read from a request body (spec §12.7).
- This service does not embed or generate in-process; those are owned by
  `retrieval` and `gpu-runtime` respectively.
- Every binary forbids `unsafe`, initializes logging/config from `diyrag-common`,
  exposes `/healthz` + `/readyz`, serves with `axum::serve`, and drains on
  SIGTERM/Ctrl-C (spec §14, §19).

## Status

Scaffold: real types, traits, signatures, and the route table are in place;
deferred logic bodies are marked `// TODO:`. Not yet wired for `cargo build`
beyond the workspace skeleton (NATS/Qdrant/Postgres calls are stubbed).
