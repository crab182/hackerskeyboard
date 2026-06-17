# ADR-0006: Per-tenant Qdrant collections
- Status: Accepted
- Date: 2026-06-17

## Context
The platform is multi-tenant across users/IPs/instances. Tenant A must never retrieve Tenant B's chunks, even with a crafted adversarial embedding query (acceptance #6). A common shortcut — one shared collection with a `tenant_id` payload filter — leaks across tenants if a single query is ever constructed without the filter, and is vulnerable to embedding-inversion/poisoned-retrieval attacks.

## Decision
Give each tenant a **separate Qdrant collection** named `t_{tenant_slug}` (hard isolation). The collection is derived **server-side from the authenticated principal**, never from a client-supplied tenant/collection parameter. Each collection uses named vectors (`dense`, `sparse`) for native hybrid search, with payload indexes on `root_id`/`retention_status`/`document_id`. Raw vectors are never exposed to clients.

## Consequences
**Easier:** isolation is structural, not a filter that can be forgotten; per-tenant snapshot/backup/replication and quotas; a red-team isolation test can assert cross-collection access is impossible.

**Harder:** many collections at large tenant counts (acceptable for a homelab/SMB; revisit at scale); sync replicates per-collection snapshots; a single-private-corpus deployment carries minor overhead (collapses to one collection — see spec §24.2).

**Follow-ups:** every retrieval path asserts a server-derived collection; isolation test gates release; combine with the `ACTIVE` retention filter and RBAC (§12.6/§12.7).

## Alternatives considered
- **Shared collection + payload filter** — cheaper, but one missing filter = a cross-tenant leak; rejected as the isolation boundary (payload filters are still used *within* a tenant for `root_id`/retention).
- **Separate Qdrant instance per tenant** — strongest isolation, far too heavy for the target hardware. Rejected.
