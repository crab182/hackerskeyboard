# diyrag-common

Shared library crate (`diyrag-common`) for the diyRAG platform. Every service
depends on it for cross-cutting concerns so behavior is identical edge-to-core.

## Modules

| Module        | Spec ref | Responsibility |
|---------------|----------|----------------|
| `config`      | §0, §19  | Typed, env-driven config (`figment`: TOML defaults + `DIYRAG_*` env overrides). No hardcoded hosts/ports/secrets/models. |
| `logging`     | §13.1    | `tracing` JSON subscriber + per-request span carrying `correlation_id`; Tower trace layer. |
| `correlation` | §13.1    | `CorrelationId` UUID newtype + Axum extractor; `X-Correlation-ID` header constant. |
| `errors`      | §11.3,§14| `AppError` (`thiserror`), `Classification {Transient, Permanent}`, and the standard `ErrorEnvelope`. |
| `auth`        | §12.2,§12.6 | argon2 key hash/verify, JWT verify, `Scope`/`DomainScope`, RBAC `Role {Reader, Ingester, Admin}`. |
| `db`          | §5.1     | `sqlx` `PgPool` init + migration helper + readiness ping. |
| `schemas`     | §5.1     | `serde` + `sqlx::FromRow` structs for every table + status/retention/structure enums. |
| `vector`      | §5.2,§7.1| `VectorStore` trait + `QdrantStore` skeleton (per-tenant collections, hybrid+RRF). |
| `blob`        | §5.3     | `object_store` wrapper + content-addressed key helper (`sha256/{first2}/{sha256}`). |
| `ids`         | §5.1     | UUIDv7 PK helper + sha256 content hashing. |

## Conventions

- `#![forbid(unsafe_code)]` at the crate root and in every module.
- Library errors use `thiserror` (never `anyhow`).
- Functions whose logic is deferred return a placeholder `AppError` and carry a
  `// TODO:` describing the intended implementation.
- Bodies are stubs; the type definitions and signatures are the contract.

## Status

Scaffold only — TODO bodies remain to be filled per the phased plan (§20).
