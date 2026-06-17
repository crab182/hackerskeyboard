# diyrag-retrieval

The hybrid retrieval service of diyRAG (binary `diyrag-retrieval`). Called by
`core-api`; not client-facing. See `MASTER_BUILD_SPEC.md` §7.

## Pipeline

1. **Embed** the query into BGE-M3 dense + sparse vectors (`hybrid.rs`,
   `embed_query`). Default backend is `gpu-runtime` `/embed`; swap to the
   in-process candle backend behind the same signature (spec §16, ADR-0009). The
   in-proc candle path yields **dense** only; **sparse** comes from `gpu-runtime`.
2. **Hybrid search** (`hybrid.rs`): a single Qdrant `query` fetches both dense
   and sparse modalities and fuses them server-side (RRF default, or weighted).
   Two invariants are enforced inside the `VectorStore`, not the request, so they
   cannot be bypassed:
   - `retention_status = ACTIVE` filter on every query (spec §6.6 / §7.1);
   - tenant-collection scoping `{prefix}{tenant_slug}` derived from the trusted
     `tenant_slug` set by core-api from the verified principal (spec §5.2 /
     §12.7).
   Returns the top-`k₀` (≈40).
3. **Rerank** (`rerank.rs`): a `bge-reranker-v2-m3` cross-encoder scores each
   `(query, chunk)` pair and keeps the top-`k` (8–12). Two interchangeable
   backends behind `RerankBackend`: in-process candle (XLM-RoBERTa
   sequence-classifier, default) or `gpu-runtime` `/rerank` (ADR-0009).
4. **Condense** (`condense.rs`, optional): a cheap LLM pass extracts only the
   query-relevant facts to prevent context-window collapse (spec §7.2). Retrieved
   text is wrapped in trust-marked `<untrusted>` delimiters and treated as data,
   not instructions (spec §12.5).

## Boundaries

- Owns no answer generation — that is `gpu-runtime`.
- The `tenant_slug` on `/search` is trusted because it is set by core-api from
  the authenticated principal; end clients never reach this service directly.
- Forbids `unsafe`; initializes logging/config from `diyrag-common`; exposes
  `/healthz` + `/readyz`; serves with `axum::serve`; drains on SIGTERM/Ctrl-C
  (spec §14, §19).

## Status

Scaffold: real types, the `RerankBackend` trait, the full search pipeline wiring,
and the Qdrant `VectorStore` calls are in place. The in-proc candle reranker
(`rerank.rs`) loads a local XLM-RoBERTa cross-encoder and runs the forward pass;
deferred logic bodies are marked `// TODO:` (query embedding, HTTP rerank backend,
condense prompt, chunk-text hydration, batched rerank).
