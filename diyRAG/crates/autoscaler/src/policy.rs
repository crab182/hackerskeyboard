#![forbid(unsafe_code)]
//! Pure scaling-decision policy (MASTER_BUILD_SPEC.md §15, §22 #7).
//!
//! [`desired_replicas`] is a **pure function** of the observed ingest queue depth,
//! the current replica count, and a typed [`ScalingConfig`]. Keeping it pure makes
//! the scaling behavior unit-testable and free of side effects — the I/O (polling
//! NATS, driving Docker Compose / Windows tasks) lives in `main.rs`.
//!
//! Design goals (§15):
//!   * **min/max clamps** — never scale below `min_replicas` (keep throughput) or
//!     above `max_replicas` (respect GPU/host limits).
//!   * **hysteresis** — separate scale-up / scale-down thresholds and a step cap
//!     so the fleet does not oscillate (flap) around a single threshold.
//!   * **query isolation** — this policy only ever scales the INGEST fleet; query
//!     subjects live on separate NATS subjects/queues and are never starved
//!     (§15 / §22 #7). The autoscaler must never be pointed at a query subject.

use serde::{Deserialize, Serialize};

/// Typed scaling configuration (§15). All values are env/config-driven (§0/§19).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingConfig {
    /// Hard floor on replicas/tasks — always keep this many warm.
    pub min_replicas: usize,
    /// Hard ceiling — respects GPU/host capacity (reference hardware, §15).
    pub max_replicas: usize,
    /// Target backlog **per replica**. Desired count ≈ depth / this. Must be > 0.
    pub target_depth_per_replica: u64,
    /// Scale UP only when queue depth exceeds this (hysteresis high-water mark).
    pub scale_up_depth: u64,
    /// Scale DOWN only when queue depth falls below this (hysteresis low-water
    /// mark). Must be ≤ `scale_up_depth` to create a stable dead-band.
    pub scale_down_depth: u64,
    /// Maximum replicas added or removed in a single decision (step cap → no
    /// thundering-herd scale events).
    pub max_step: usize,
}

impl Default for ScalingConfig {
    /// Conservative defaults sized for the reference single-GPU node (§15).
    fn default() -> Self {
        Self {
            min_replicas: 1,
            max_replicas: 8,
            target_depth_per_replica: 200,
            scale_up_depth: 500,
            scale_down_depth: 100,
            max_step: 2,
        }
    }
}

impl ScalingConfig {
    /// Validate invariants the policy relies on. Returns a message on violation so
    /// `main` can fail fast rather than scale on nonsense config (§0).
    pub fn validate(&self) -> Result<(), String> {
        if self.min_replicas > self.max_replicas {
            return Err("min_replicas must be <= max_replicas".to_owned());
        }
        if self.target_depth_per_replica == 0 {
            return Err("target_depth_per_replica must be > 0".to_owned());
        }
        if self.scale_down_depth > self.scale_up_depth {
            return Err("scale_down_depth must be <= scale_up_depth".to_owned());
        }
        if self.max_step == 0 {
            return Err("max_step must be >= 1".to_owned());
        }
        Ok(())
    }
}

/// Compute the desired replica/task count for the ingestion-worker fleet (§15).
///
/// Inputs:
///   * `depth`   — current ingest-subject backlog (pending+unacked messages),
///   * `rate`    — recent processing rate (msgs/sec) across the fleet; reserved
///     for a future predictive term and currently used only to break the
///     dead-band toward stability (see below),
///   * `current` — current replica/task count,
///   * `cfg`     — clamps + hysteresis thresholds.
///
/// Behavior:
///   * In the dead-band (`scale_down_depth <= depth <= scale_up_depth`) → hold
///     `current` (hysteresis: no flapping).
///   * Above `scale_up_depth` → move toward `ceil(depth / target_depth_per_replica)`,
///     capped by `max_step` and `max_replicas`.
///   * Below `scale_down_depth` → step down by at most `max_step`, floored at
///     `min_replicas`. If the fleet is idle (`depth == 0` and `rate == 0`) the
///     target collapses to `min_replicas` (still via the step cap).
///
/// Always clamped to `[min_replicas, max_replicas]`.
#[must_use]
pub fn desired_replicas(depth: u64, rate: f64, current: usize, cfg: &ScalingConfig) -> usize {
    // Defensive: a misconfigured target would divide-by-zero; treat as "hold".
    if cfg.target_depth_per_replica == 0 {
        return clamp(current, cfg);
    }

    let target = if depth > cfg.scale_up_depth {
        // Scale up toward the per-replica target, but never jump more than max_step.
        let ideal = depth.div_ceil(cfg.target_depth_per_replica) as usize;
        let stepped_up = current.saturating_add(cfg.max_step);
        ideal.min(stepped_up)
    } else if depth < cfg.scale_down_depth {
        // Scale down by at most max_step. An idle fleet trends to the floor.
        let _ = rate; // reserved: a non-zero rate could damp the step further.
        current.saturating_sub(cfg.max_step)
    } else {
        // Dead-band → hold (hysteresis, §15).
        current
    };

    clamp(target, cfg)
}

/// Clamp a candidate count to `[min_replicas, max_replicas]` (§15).
#[must_use]
fn clamp(n: usize, cfg: &ScalingConfig) -> usize {
    n.clamp(cfg.min_replicas, cfg.max_replicas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ScalingConfig {
        ScalingConfig {
            min_replicas: 1,
            max_replicas: 8,
            target_depth_per_replica: 200,
            scale_up_depth: 500,
            scale_down_depth: 100,
            max_step: 2,
        }
    }

    #[test]
    fn default_config_is_valid() {
        assert!(ScalingConfig::default().validate().is_ok());
    }

    #[test]
    fn dead_band_holds_current() {
        let c = cfg();
        // depth within [100, 500] → hold whatever we have.
        assert_eq!(desired_replicas(100, 1.0, 3, &c), 3);
        assert_eq!(desired_replicas(300, 1.0, 3, &c), 3);
        assert_eq!(desired_replicas(500, 1.0, 3, &c), 3);
    }

    #[test]
    fn scales_up_but_respects_step_cap() {
        let c = cfg();
        // depth 2000 → ideal = ceil(2000/200) = 10, but step cap from 2 is +2 → 4,
        // then clamped to max 8 → 4.
        assert_eq!(desired_replicas(2000, 5.0, 2, &c), 4);
    }

    #[test]
    fn scales_up_clamped_to_max() {
        let c = cfg();
        // From 7, ideal high, step → 9, clamped to max 8.
        assert_eq!(desired_replicas(5000, 5.0, 7, &c), 8);
    }

    #[test]
    fn scales_down_but_respects_step_cap_and_floor() {
        let c = cfg();
        // depth 10 < 100 → step down by 2: from 5 → 3.
        assert_eq!(desired_replicas(10, 0.1, 5, &c), 3);
        // From 2 → 0 stepped, but floored at min 1.
        assert_eq!(desired_replicas(0, 0.0, 2, &c), 1);
    }

    #[test]
    fn idle_fleet_trends_to_floor_over_repeated_calls() {
        let c = cfg();
        // Simulate the control loop: empty queue should walk down to the floor.
        let mut n = 8;
        for _ in 0..10 {
            n = desired_replicas(0, 0.0, n, &c);
        }
        assert_eq!(n, c.min_replicas);
    }

    #[test]
    fn never_below_min_or_above_max() {
        let c = cfg();
        for depth in [0u64, 50, 99, 100, 500, 501, 10_000] {
            for current in 0..=12 {
                let d = desired_replicas(depth, 1.0, current, &c);
                assert!(d >= c.min_replicas, "below min at depth={depth} current={current}");
                assert!(d <= c.max_replicas, "above max at depth={depth} current={current}");
            }
        }
    }

    #[test]
    fn hysteresis_prevents_flapping_at_a_single_threshold() {
        // With a dead-band, oscillating depth around one point does not flap:
        // bounce between 120 and 480 (both inside the band) holds steady.
        let c = cfg();
        let mut n = 4;
        for depth in [120u64, 480, 120, 480, 300] {
            n = desired_replicas(depth, 1.0, n, &c);
            assert_eq!(n, 4);
        }
    }

    #[test]
    fn invalid_config_is_rejected() {
        let mut c = cfg();
        c.min_replicas = 9; // > max
        assert!(c.validate().is_err());

        let mut c2 = cfg();
        c2.scale_down_depth = 600; // > scale_up_depth
        assert!(c2.validate().is_err());

        let mut c3 = cfg();
        c3.target_depth_per_replica = 0;
        assert!(c3.validate().is_err());
    }
}
