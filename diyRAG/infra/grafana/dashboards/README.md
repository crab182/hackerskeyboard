# Grafana dashboards

Dashboard JSON for diyRAG lives here and is provisioned into Grafana (read from
Tempo + Prometheus + Loki, all fed by the OTel Collector — `../../otel/`).
The panels below realise the **§13.3 minimum metric set** from
`MASTER_BUILD_SPEC.md`. Build the dashboards (or import JSON) to cover exactly
these signals; this README is the authoritative panel checklist.

## Provisioning

Mount this directory and a provisioning config into Grafana
(`/etc/grafana/provisioning/dashboards/`). Datasources (Prometheus, Tempo, Loki)
are provisioned separately; endpoints come from env / Docker secrets (no secrets
committed — §17).

## Panels (§13.3 — the required metric minimum)

### 1. Ingestion & queue health
- **Queue depth** — NATS JetStream pending/ack-pending per subject (interactive
  vs bulk priority subjects — §15). Alert when bulk depth drives the autoscaler.
- **Ingestion rate** — documents/sec moving to `INDEXED`; failures/sec to
  `QUARANTINED`/DLQ.
- **Parse / embed / index latency** — p50/p95/p99 histograms per stage (§6).

### 2. Retrieval & answering (SLA panels — §1)
- **Retrieval latency p50/p95** — gate at p95 < 800 ms (acceptance #2).
- **Answer latency p95** — gate at p95 < 6 s (acceptance #2).
- **Per-tenant QPS** — query throughput by tenant (§13.3).

### 3. Errors
- **Error rate by class** — `TRANSIENT` vs `PERMANENT` (§14 taxonomy), by
  `service_name` and `level` (from `error_log` / OTel logs).
- **Quarantine / DLQ size** — current depth + re-inject trend (§14).

### 4. GPU & hardware (§16)
- **GPU utilization** — % per device.
- **VRAM usage** — used/total per device; flag approaching the model limit.
- **GPU temperature** — with the thermal-throttle threshold marked; ties to the
  `HW-THERMAL-LIMIT` / `HW-OOM` fallback events (§14, §16).

### 5. Sync (LAN multi-instance — §9)
- **Sync lag** — convergence delay / outstanding records per peer.
- **Peer status** — `nodes.last_seen`, cert-pin status, priority.

### 6. Service / runtime health (§13.3, §16b)
- **Service uptime** — per service.
- **Restart count** — Windows Service / container restarts (acceptance #9
  reboot-survival signal).
- **Health/Ready** — `/healthz` + `/readyz` status per service.

## Conventions
- Every panel filters by `deployment.environment` (set by the OTel `resource`
  processor) and, where relevant, `tenant_id`.
- Trace exemplars link latency panels to Tempo spans via the propagated
  `correlation_id` (§13.1).
- Log panels (Loki) pivot on `correlation_id` so a spike opens the exact request
  trail, and on `reference_code` (= `error_log.log_id`) for error deep-links
  (§10.4, acceptance #8).

## Files
- `*.json` — dashboard exports (add as built). Keep one dashboard per section
  above, or a single "diyRAG Overview" with row groups matching the sections.
