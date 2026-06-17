-- diyRAG — initial schema (MASTER_BUILD_SPEC.md §5.1).
-- sqlx migration. Applied by `sqlx migrate run` / `just migrate` (see README.md).
--
-- Conventions (§5.1, §19):
--   * All PKs are UUID (UUIDv7 generated in Rust via the `uuid` crate; sortable).
--     We do NOT default-generate in SQL so Rust owns id generation/ordering.
--   * All timestamps are `timestamptz`, stored UTC.
--   * JSONB for flexible/semi-structured fields (scopes, globs, version vectors).
--   * Parameterized queries only at the app layer (§12.4) — never string concat.
--
-- This schema is the LAN-sync integration contract (§5): kept identical so a
-- Rust node and a hypothetical Python node could interoperate. Do not diverge
-- column names/enums without a coordinated migration on every peer.

-- ---------------------------------------------------------------------------
-- Extensions
-- ---------------------------------------------------------------------------
-- pgcrypto provides gen_random_uuid() as a fallback for any server-side default
-- needs; the app generates UUIDv7 itself for sortable PKs (§5.1).
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Enums (§5.1)
-- ---------------------------------------------------------------------------

-- documents.status (§5.1)
CREATE TYPE document_status AS ENUM (
  'PENDING',
  'PARSING',
  'CHUNKING',
  'EMBEDDING',
  'INDEXED',
  'QUARANTINED'
);

-- documents.retention_status (§5.1, §6.6 logical purge)
CREATE TYPE retention_status AS ENUM (
  'ACTIVE',
  'PURGED_LOGICAL'
);

-- chunks.structure_type (§5.1, §6.4)
CREATE TYPE structure_type AS ENUM (
  'prose',
  'table',
  'heading',
  'code',
  'triple'
);

-- jobs.type (§5.1)
CREATE TYPE job_type AS ENUM (
  'BATCH',
  'REINDEX',
  'SYNC'
);

-- jobs.status (§5.1)
CREATE TYPE job_status AS ENUM (
  'PENDING',
  'RUNNING',
  'COMPLETE',
  'FAILED',
  'PARTIAL_FAILURE'
);

-- work_units.state (§5.1, §14)
CREATE TYPE work_unit_state AS ENUM (
  'QUEUED',
  'IN_PROGRESS',
  'SUCCESS',
  'FAILURE',
  'FAILED_RECOVERABLE',
  'DLQ'
);

-- error_log.level (§13.2). Defined here so both 0001 and the partitioned
-- error_log in 0002 share one type.
CREATE TYPE error_level AS ENUM (
  'DEBUG',
  'INFO',
  'WARN',
  'ERROR',
  'CRITICAL'
);

-- users.status — account lifecycle.
CREATE TYPE user_status AS ENUM (
  'ACTIVE',
  'SUSPENDED',
  'DISABLED'
);

-- ---------------------------------------------------------------------------
-- Tenancy & identity (§5.1)
-- ---------------------------------------------------------------------------

-- Isolation boundary. One Qdrant collection per tenant (`t_{slug}`) — §5.2/§12.7.
CREATE TABLE tenants (
  id          UUID PRIMARY KEY,
  name        TEXT        NOT NULL,
  slug        TEXT        NOT NULL UNIQUE,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
  id            UUID PRIMARY KEY,
  tenant_id     UUID        NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  email         TEXT        NOT NULL UNIQUE,
  display_name  TEXT,
  status        user_status NOT NULL DEFAULT 'ACTIVE',
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_users_tenant ON users (tenant_id);

-- API keys: store ONLY an argon2 salted hash, never the raw key (§5.1, §12.2).
-- `scopes` = resource perms; `domain_scope` = which collections/roots allowed.
CREATE TABLE api_keys (
  id            UUID PRIMARY KEY,
  tenant_id     UUID        NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  user_id       UUID        REFERENCES users(id) ON DELETE SET NULL,
  key_hash      TEXT        NOT NULL,          -- argon2 hash (§12.2)
  prefix        TEXT        NOT NULL,          -- non-secret lookup prefix
  scopes        JSONB       NOT NULL DEFAULT '[]'::jsonb,
  domain_scope  JSONB       NOT NULL DEFAULT '{}'::jsonb,
  expires_at    TIMESTAMPTZ,
  revoked_at    TIMESTAMPTZ,                   -- instant revocation (§12.2)
  last_used_at  TIMESTAMPTZ,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_api_keys_tenant ON api_keys (tenant_id);
CREATE INDEX idx_api_keys_prefix ON api_keys (prefix);

-- RBAC (§12.6).
CREATE TABLE roles (
  id    UUID PRIMARY KEY,
  name  TEXT NOT NULL UNIQUE      -- 'reader' | 'ingester' | 'admin' (baseline §12.6)
);

CREATE TABLE user_roles (
  user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role_id  UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, role_id)
);

-- ---------------------------------------------------------------------------
-- Corpus: roots, documents, chunks (§5.1)
-- ---------------------------------------------------------------------------

-- Watched folder roots (§6.1). include/exclude globs as JSONB arrays.
CREATE TABLE roots (
  id             UUID PRIMARY KEY,
  tenant_id      UUID        NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  path           TEXT        NOT NULL,
  description    TEXT,
  is_active      BOOLEAN     NOT NULL DEFAULT TRUE,
  watch          BOOLEAN     NOT NULL DEFAULT FALSE,
  include_globs  JSONB       NOT NULL DEFAULT '[]'::jsonb,
  exclude_globs  JSONB       NOT NULL DEFAULT '[]'::jsonb,
  source_root_id UUID        REFERENCES roots(id) ON DELETE SET NULL,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_roots_tenant ON roots (tenant_id);
CREATE INDEX idx_roots_active ON roots (tenant_id, is_active);

CREATE TABLE documents (
  id                UUID PRIMARY KEY,
  tenant_id         UUID             NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  root_id           UUID             REFERENCES roots(id) ON DELETE SET NULL,
  source_path       TEXT             NOT NULL,
  content_sha256    TEXT             NOT NULL,                 -- content addressing (§5.3)
  mime              TEXT,
  bytes             BIGINT,
  parser            TEXT,                                      -- handler used (§6.3)
  status            document_status  NOT NULL DEFAULT 'PENDING',
  retention_status  retention_status NOT NULL DEFAULT 'ACTIVE',
  version_vector    JSONB            NOT NULL DEFAULT '{}'::jsonb,  -- CRDT (§9)
  lang              TEXT,
  page_count        INTEGER,
  error_ref         UUID,                                      -- -> error_log.log_id (§13.2)
  blob_key          TEXT,                                      -- object_store key (§5.3)
  created_at        TIMESTAMPTZ      NOT NULL DEFAULT now(),
  indexed_at        TIMESTAMPTZ,
  updated_at        TIMESTAMPTZ      NOT NULL DEFAULT now(),
  -- Dedup / idempotency per tenant on content hash (§5.1, §6.2).
  CONSTRAINT uq_documents_tenant_sha UNIQUE (tenant_id, content_sha256)
);
CREATE INDEX idx_documents_tenant_status ON documents (tenant_id, status);
CREATE INDEX idx_documents_root ON documents (root_id);
CREATE INDEX idx_documents_retention ON documents (tenant_id, retention_status);

CREATE TABLE chunks (
  id              UUID PRIMARY KEY,
  document_id     UUID           NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  tenant_id       UUID           NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  ordinal         INTEGER        NOT NULL,
  text            TEXT           NOT NULL,
  token_count     INTEGER,
  section_heading TEXT,
  page_number     INTEGER,
  structure_type  structure_type NOT NULL DEFAULT 'prose',
  embed_model     TEXT,                                      -- drift detection (§7.3, §9)
  vector_id       UUID           NOT NULL,                   -- mirrors Qdrant point id (§5.2)
  created_at      TIMESTAMPTZ    NOT NULL DEFAULT now(),
  CONSTRAINT uq_chunks_doc_ordinal UNIQUE (document_id, ordinal)
);
CREATE INDEX idx_chunks_document ON chunks (document_id);
CREATE INDEX idx_chunks_tenant ON chunks (tenant_id);
-- Postgres <-> Qdrant joinable both directions (§5.2).
CREATE UNIQUE INDEX uq_chunks_vector_id ON chunks (vector_id);

-- ---------------------------------------------------------------------------
-- Jobs & work units (§5.1, §6.7, §14)
-- ---------------------------------------------------------------------------

CREATE TABLE jobs (
  id                 UUID PRIMARY KEY,
  tenant_id          UUID        NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  type               job_type    NOT NULL,
  status             job_status  NOT NULL DEFAULT 'PENDING',
  total_units        INTEGER     NOT NULL DEFAULT 0,
  processed_count    INTEGER     NOT NULL DEFAULT 0,
  failed_unit_count  INTEGER     NOT NULL DEFAULT 0,
  threshold_pct      INTEGER     NOT NULL DEFAULT 20,        -- PARTIAL_FAILURE gate (§6.7)
  created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at        TIMESTAMPTZ
);
CREATE INDEX idx_jobs_tenant_status ON jobs (tenant_id, status);

CREATE TABLE work_units (
  id              UUID PRIMARY KEY,
  job_id          UUID            NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  document_ref    TEXT,                                      -- path/uri of the unit's file
  content_sha256  TEXT            NOT NULL,                  -- idempotency key (§6.2)
  state           work_unit_state NOT NULL DEFAULT 'QUEUED',
  retry_count     INTEGER         NOT NULL DEFAULT 0,
  last_error_ref  UUID,                                      -- -> error_log.log_id
  claimed_by      TEXT,                                      -- worker id (§6.2)
  claimed_at      TIMESTAMPTZ
);
CREATE INDEX idx_work_units_job ON work_units (job_id);
CREATE INDEX idx_work_units_state ON work_units (state);
-- Stale-claim reclaim scan (§14 crash recovery).
CREATE INDEX idx_work_units_inprogress ON work_units (state, claimed_at)
  WHERE state = 'IN_PROGRESS';

-- ---------------------------------------------------------------------------
-- Audit (§5.1, §12.9). Append-only by policy (no UPDATE/DELETE from app).
-- ---------------------------------------------------------------------------
CREATE TABLE audit_log (
  id              UUID PRIMARY KEY,
  tenant_id       UUID,
  actor_user_id   UUID,
  actor_key_id    UUID,
  action          TEXT        NOT NULL,
  resource_type   TEXT,
  resource_id     TEXT,
  before          JSONB,
  after           JSONB,
  ip              INET,
  correlation_id  UUID,
  at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_audit_tenant_at ON audit_log (tenant_id, at);
CREATE INDEX idx_audit_correlation ON audit_log (correlation_id);

-- ---------------------------------------------------------------------------
-- LAN sync (§5.1, §9)
-- ---------------------------------------------------------------------------

-- CRDT registry state keyed by record (content hash). version_vector = {node:ctr}.
CREATE TABLE sync_state (
  record_key      TEXT        NOT NULL,                      -- e.g. content_sha256
  tenant_id       UUID        NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  version_vector  JSONB       NOT NULL DEFAULT '{}'::jsonb,
  last_hash       TEXT,
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  origin_node     TEXT,
  PRIMARY KEY (tenant_id, record_key)
);

-- Known LAN peers; cert-pinned (§9). priority drives deterministic tiebreak.
CREATE TABLE nodes (
  id                UUID PRIMARY KEY,
  name              TEXT        NOT NULL UNIQUE,
  priority          INTEGER     NOT NULL DEFAULT 0,
  last_seen         TIMESTAMPTZ,
  cert_fingerprint  TEXT,                                    -- pinned on enrolment (§9)
  endpoint          TEXT
);

-- ---------------------------------------------------------------------------
-- Baseline RBAC roles (§12.6). Ids are deterministic UUIDs for idempotent seed.
-- ---------------------------------------------------------------------------
INSERT INTO roles (id, name) VALUES
  ('00000000-0000-0000-0000-0000000000a1', 'reader'),
  ('00000000-0000-0000-0000-0000000000a2', 'ingester'),
  ('00000000-0000-0000-0000-0000000000a3', 'admin')
ON CONFLICT (name) DO NOTHING;
