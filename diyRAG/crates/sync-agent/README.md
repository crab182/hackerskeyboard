# diyrag-sync-agent

LAN multi-instance sync daemon — **MASTER_BUILD_SPEC.md §9** (and red-team §22 #4, #11).

Makes cooperating diyRAG instances on a LAN converge on (a) the processed-file
**registry** and (b) the retrievable **vector corpus**, over mutually-authenticated
TLS, surviving partitions (acceptance criterion #5).

## Binary

`diyrag-sync-agent` — runs three cancellable loops:

| Module | Responsibility |
|---|---|
| `discovery.rs` | mDNS (`mdns-sd`) browse + static peer list; peers stay `Pending` until an **admin pins the cert fingerprint** (no auto-trust). |
| `crdt.rs` | Version-vector (`{node:counter}`) conflict resolution: dominance check, else deterministic tiebreak (**highest node priority, then lexicographically smallest node id**). **No wall-clock LWW.** |
| `replication.rs` | Qdrant snapshot pull/apply; re-embed from blobs on embedding-model drift; content-addressed blob fetch by hash. |
| `grpc.rs` | tonic server + client over **mTLS (rustls)** with cert pinning; `governor` token-bucket rate-limit; diffs-vs-last-acked-manifest only; bounded record counts. |

## Wire protocol

`proto/sync.proto` (compiled by `build.rs` via `tonic-build`, both server + client)
sketches the RPCs: `ExchangeRegistryDiff`, `ListSnapshots`, `FetchSnapshot`,
`FetchBlob`, `Health`.

## Conflict resolution (the load-bearing invariant)

Identity is `content_sha256` and writes are idempotent, so most "conflicts" are
no-ops. Genuine concurrent edits resolve deterministically so **every peer reaches
the same winner without coordination** — see the `#[cfg(test)]` suite in
`crdt.rs` (`resolution_is_symmetric_and_deterministic`). Clocks are never trusted.

## Security

- Mutual TLS, rcgen-issued ≤90-day certs (§12.1); unknown certs rejected by a
  cert-pinning rustls verifier.
- Token-bucket DoS guard + bounded per-sync record counts (§9 / §22 #4).
- Replicated payloads are treated as data, never instructions (§12.5).

## Status

Scaffold: real types/signatures and the conflict-resolution + drift-plan logic
(with unit tests) are implemented; network/Qdrant/blob bodies are marked
`// TODO:`. `proto` compilation is guarded in `build.rs` until `protoc` is wired
into CI (matches the `ingestion-worker` pattern).
