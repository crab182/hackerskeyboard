# Architecture Decision Records (ADRs)

This folder captures the significant, hard-to-reverse decisions behind diyRAG. Each `DECISION:` made during the build should land here.

## Format

Each ADR uses:

```
# ADR-NNNN: <title>
- Status: Proposed | Accepted | Superseded by ADR-XXXX
- Date: YYYY-MM-DD

## Context
What forces are at play; what problem we are solving.

## Decision
What we decided to do.

## Consequences
What becomes easier/harder; trade-offs accepted; follow-ups.

## Alternatives considered
Options we rejected and why.
```

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](./0001-rust-first-service-tier.md) | Rust-first service tier | Accepted |
| [0002](./0002-python-confined-to-inference-and-hard-parsing.md) | Python confined to inference & hard parsing | Accepted |
| [0003](./0003-windows-service-and-unraid-runtime.md) | Windows Service + unraid dual runtime | Accepted |
| [0004](./0004-rust-native-inference-default-vllm-optional.md) | Rust-native inference default, vLLM optional | Accepted |
| [0005](./0005-version-vector-crdt-no-lww.md) | Version-vector CRDT, no wall-clock LWW | Accepted |
| [0006](./0006-per-tenant-qdrant-collections.md) | Per-tenant Qdrant collections | Accepted |
| [0007](./0007-socket-security-triage.md) | Socket Security triage (CVE bumps + obfuscated-code false positives) | Accepted |
| [0008](./0008-ort-load-dynamic-no-openssl.md) | ort load-dynamic — no OpenSSL, no build-time download | Superseded by 0009 |
| [0009](./0009-candle-replaces-ort.md) | candle replaces ort for in-proc embeddings + reranking | Accepted |
