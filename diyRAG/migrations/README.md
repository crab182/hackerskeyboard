# migrations (sqlx, SQL-first)

PostgreSQL 16 schema for diyRAG, applied with **`sqlx migrate`** (SQL files,
in-repo, reviewable — `MASTER_BUILD_SPEC.md` §3.2, §5.1). This is the
authoritative metadata store and the **LAN-sync integration contract** (§5):
column names and enums are kept stable so peers interoperate.

## Files

| File | Contents | Spec |
|---|---|---|
| `0001_init.sql` | enums + tenancy/identity/RBAC + roots/documents/chunks + jobs/work_units + audit_log + sync_state/nodes; seeds baseline roles | §5.1, §6, §9, §12.6 |
| `0002_error_log_partitions.sql` | append-only `error_log` partitioned by month + partition helper functions | §13.2 |

### Enums (0001)
`document_status`, `retention_status`, `structure_type`, `job_type`,
`job_status`, `work_unit_state`, `error_level`, `user_status`.

### Key invariants
- All PKs are **UUID** (UUIDv7 generated in Rust via the `uuid` crate — sortable;
  not server-defaulted, so Rust owns id generation — §5.1).
- All timestamps are `timestamptz` (**UTC**).
- `documents` has `UNIQUE (tenant_id, content_sha256)` enforcing dedup /
  idempotency (§5.1, §6.2).
- `chunks.vector_id` is `UNIQUE` and mirrors the Qdrant point id — Postgres ⇄
  Qdrant joinable both ways (§5.2).
- `error_log.log_id` is the **`reference_code`** surfaced in every client
  (§11.3, §10.4, acceptance #8).
- `error_log` is **append-only** and **range-partitioned by month**; indexes on
  the parent propagate to partitions. A `DEFAULT` partition is the safety net.

## Applying

### sqlx CLI
```bash
# install once: cargo install sqlx-cli --no-default-features --features rustls,postgres
export DATABASE_URL=postgres://USER:PASS@HOST:5432/diyrag   # no secret committed (§17)
sqlx migrate run --source migrations
sqlx migrate info --source migrations     # show applied/pending
# revert is not provided (forward-only SQL); roll forward with a new file.
```

`sqlx migrate run` records applied versions in the `_sqlx_migrations` table and
applies files in lexical order (`0001_…`, `0002_…`).

### Makefile / justfile (preferred)
The repo `justfile`/`Makefile` wraps the above so first-run bootstrap (§17) runs
migrations after Postgres is healthy:
```bash
just migrate          # -> sqlx migrate run --source migrations
just db-reset         # drop + recreate + migrate (dev only)
```
On first-run bootstrap the supervisor (`diyragd`) also runs migrations before
starting services (§17).

### Compile-time checked queries
The Rust `common` crate uses `sqlx::query!`, which checks SQL against this schema
at **build time**. After changing a migration, run `cargo sqlx prepare` (offline
mode) so CI can compile without a live database.

## Partition maintenance (error_log)

`0002` creates `ensure_error_log_partition(month DATE)` and
`ensure_error_log_partitions_current_and_next()`, and materialises the current +
next month at migration time. Schedule the helper monthly (cron / a `diyrag`
maintenance task) so there is always a live partition ahead of `now()`:
```sql
SELECT ensure_error_log_partitions_current_and_next();
```
Rows that land in `error_log_default` indicate a missing partition — investigate
and migrate them out.

## Adding a migration
1. Create `migrations/NNNN_short_description.sql` with the next zero-padded number.
2. Forward-only; never edit an applied migration (peers may have run it — §5/§9).
3. Keep the sync-contract columns/enums stable; coordinate any contract change
   across all LAN peers.
4. Re-run `cargo sqlx prepare` and commit the updated `.sqlx/` query cache.
