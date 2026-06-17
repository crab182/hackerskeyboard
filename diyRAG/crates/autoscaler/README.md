# diyrag-autoscaler

Queue-depth-driven ingestion-worker scaler — **MASTER_BUILD_SPEC.md §15** (red-team §22 #7).

Polls the **ingest** NATS JetStream subject's queue depth and scales the
`ingestion-worker` fleet between min/max with hysteresis. This is the Rust analog
of a Kubernetes HPA but driven by broker backlog (§3.2 note / §15).

## Binary

`diyrag-autoscaler` — runs a poll → decide → reconcile control loop:

1. **Observe** the ingest backlog via a `QueueDepthSource` (JetStream consumer
   `num_pending` + `num_ack_pending`). It observes the **ingest subject only** —
   query subjects are on separate subjects/queues and are never sampled or
   scaled here, so queries are never starved by bulk ingestion (§22 #7).
2. **Decide** with the pure `policy::desired_replicas(depth, rate, current, cfg)`.
3. **Reconcile** via a platform `Scaler`:
   - Linux/unraid → `docker compose up -d --scale ingestion-worker=N` (§16b.3),
   - Windows → adjust `diyragd` ingestion task count (§16b.1).

## `policy.rs` — the pure decision function

`desired_replicas` applies:

- **min/max clamps** (`min_replicas`/`max_replicas`),
- **hysteresis**: a dead-band between `scale_down_depth` and `scale_up_depth`
  where the fleet holds steady (no flapping), and
- a **step cap** (`max_step`) so a single decision never adds/removes more than N.

It is side-effect-free and covered by `#[cfg(test)]` unit tests: dead-band hold,
step-capped scale up/down, clamping to min/max, idle-fleet trend to floor, and an
anti-flapping check.

## Status

Scaffold: the policy function and config validation are fully implemented and
unit-tested; the NATS observation and Docker/Windows reconciliation bodies are
marked `// TODO:`. The control loop is wired and tested with fakes.
