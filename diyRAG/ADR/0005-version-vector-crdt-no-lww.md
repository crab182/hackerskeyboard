# ADR-0005: Version-vector CRDT, no wall-clock LWW
- Status: Accepted
- Date: 2026-06-17

## Context
Cooperating LAN instances must converge on the processed-file registry and the retrievable corpus after partitions heal (acceptance #5). The original plan resolved conflicts with a wall-clock "±50 ms / highest-trust" last-write-wins rule, which is unsafe under clock skew across homelab machines.

## Decision
Model the registry as a CRDT-style log keyed by `content_sha256`, where each record carries a **version vector** `{node_id: counter}`. Conflict resolution is deterministic and clock-free:
1. If one version vector causally **dominates** the other, take the dominant record.
2. Otherwise (concurrent edits) apply a deterministic tiebreak: **highest `nodes.priority`, then lexicographically smallest `node_id`.**

Because document identity is the content hash and ingestion writes are idempotent, most "conflicts" are no-ops. The resolution function is pure and unit-tested (`crates/sync-agent/src/crdt.rs`). Vectors replicate via Qdrant snapshots (or re-embed from synced blobs when `embed_model` differs); blobs are fetched by content hash. **No wall-clock comparison is ever used for correctness.**

## Consequences
**Easier:** convergence is deterministic and reproducible regardless of clock skew; idempotent hashing collapses most conflicts; the pure resolver is trivially testable.

**Harder:** version vectors grow with the number of nodes (bounded for a LAN homelab; prune retired nodes); operators must set sensible `priority` values; snapshot replication needs `embed_model` matching or a re-embed path.

**Follow-ups:** rate-limit sync (token bucket), accept diffs-vs-last-acked-manifest only, bound per-sync record counts (§9, §22 #4); cert-pin peers before sync.

## Alternatives considered
- **Wall-clock LWW** — simple but corrupts state under skew. Rejected.
- **Full CRDT lib (`automerge`)** — richer semantics than needed for a hash-keyed idempotent registry; kept as an option if document-level collaborative editing is ever added.
