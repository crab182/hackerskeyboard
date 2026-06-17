-- diyRAG — append-only error_log, partitioned by month (MASTER_BUILD_SPEC.md §13.2).
-- sqlx migration (runs after 0001_init.sql). The `error_level` enum is defined
-- in 0001 so it is shared.
--
-- log_id is the PK and IS the human-facing `reference_code` surfaced in every
-- client (§11.3, §10.4, acceptance #8). Append-only by policy: the app issues
-- INSERTs only; no UPDATE/DELETE.

-- ---------------------------------------------------------------------------
-- Parent partitioned table (RANGE on timestamp, monthly). Composite PK includes
-- the partition key (Postgres requires the partition column in the PK/unique).
-- ---------------------------------------------------------------------------
CREATE TABLE error_log (
  log_id          UUID        NOT NULL,                  -- = reference_code (§11.3)
  "timestamp"     TIMESTAMPTZ NOT NULL DEFAULT now(),    -- partition key; indexed (§13.2)
  level           error_level NOT NULL,
  service_name    VARCHAR(64) NOT NULL,                  -- originating service (§13.2)
  user_id         VARCHAR(64),                           -- nullable (§13.2)
  api_key_id      VARCHAR(64),                           -- nullable (§13.2)
  correlation_id  UUID        NOT NULL,                  -- mandatory; full reconstruction (§13.1)
  transaction_id  VARCHAR(128),                          -- content hash / job id (§13.2)
  message         TEXT        NOT NULL,
  stack_trace     JSONB,                                 -- Rust backtrace / error chain (§13.2)
  context         JSONB,                                 -- PII-scrubbed request params (§13.2)
  PRIMARY KEY (log_id, "timestamp")
) PARTITION BY RANGE ("timestamp");

-- Indexes on the parent propagate to every partition.
CREATE INDEX idx_error_log_ts ON error_log ("timestamp");
CREATE INDEX idx_error_log_correlation ON error_log (correlation_id);
CREATE INDEX idx_error_log_level ON error_log (level);
CREATE INDEX idx_error_log_service ON error_log (service_name);
-- Fast lookup by reference_code (the UI deep-link path — §10.4, acceptance #8).
CREATE INDEX idx_error_log_logid ON error_log (log_id);

-- ---------------------------------------------------------------------------
-- DEFAULT partition: a safety net so an INSERT never fails if the month's
-- partition was not pre-created. The maintenance helper below should keep the
-- current + next month materialised; rows in DEFAULT should be migrated out.
-- ---------------------------------------------------------------------------
CREATE TABLE error_log_default PARTITION OF error_log DEFAULT;

-- ---------------------------------------------------------------------------
-- Partition maintenance helper. Creates the monthly partition covering a given
-- date if it does not already exist. Call with the 1st of the target month.
-- A scheduled job (cron / `diyrag` maintenance task) calls
-- `SELECT ensure_error_log_partition(date_trunc('month', now())::date);`
-- and again for next month so there is always a live partition ahead.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ensure_error_log_partition(p_month DATE)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
  v_start   DATE := date_trunc('month', p_month)::date;
  v_end     DATE := (date_trunc('month', p_month) + INTERVAL '1 month')::date;
  v_suffix  TEXT := to_char(v_start, 'YYYY_MM');
  v_name    TEXT := format('error_log_%s', v_suffix);
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_class WHERE relname = v_name
  ) THEN
    EXECUTE format(
      'CREATE TABLE %I PARTITION OF error_log FOR VALUES FROM (%L) TO (%L);',
      v_name, v_start, v_end
    );
  END IF;
END;
$$;

-- Convenience: ensure current and next month exist at migration time.
CREATE OR REPLACE FUNCTION ensure_error_log_partitions_current_and_next()
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
  PERFORM ensure_error_log_partition(date_trunc('month', now())::date);
  PERFORM ensure_error_log_partition((date_trunc('month', now()) + INTERVAL '1 month')::date);
END;
$$;

-- Materialise current + next month now so the platform has live partitions
-- immediately after migrating (idempotent).
SELECT ensure_error_log_partitions_current_and_next();
