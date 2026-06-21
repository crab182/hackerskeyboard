#![forbid(unsafe_code)]
//! `diyrag-autoscaler` — queue-depth-driven ingestion-worker scaler (MASTER_BUILD_SPEC.md §15).
//!
//! Control loop: poll the **ingest** NATS JetStream subject's queue depth →
//! decide a target replica/task count with the pure [`policy::desired_replicas`]
//! function (clamps + hysteresis) → reconcile the fleet via a platform
//! [`Scaler`]:
//!   * **Linux/unraid** → `docker compose up -d --scale ingestion-worker=N` (§15/§16b.3),
//!   * **Windows**      → adjust the `diyragd` ingestion task count (§16b.1).
//!
//! Query subjects are kept on **separate** NATS subjects/queues from ingest
//! subjects; this autoscaler only ever observes and scales the INGEST path, so
//! interactive queries are never starved by bulk ingestion (§15 / §22 #7).
//!
//! Errors use `anyhow` at the binary boundary (spec §19).

mod policy;

use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use diyrag_common::config::AppConfig;
use diyrag_common::logging;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::policy::{desired_replicas, ScalingConfig};

/// Default poll interval between scaling decisions (§15). Config-overridable.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Observed state of the ingest queue at one poll (§15).
#[derive(Debug, Clone, Copy)]
pub struct QueueObservation {
    /// Pending + unacked messages on the ingest subject.
    pub depth: u64,
    /// Recent processing rate across the fleet (msgs/sec).
    pub rate: f64,
}

/// Reads the ingest-subject backlog from NATS JetStream (§15). Trait so it can be
/// faked in tests and so the query path is provably never observed here.
#[async_trait]
pub trait QueueDepthSource: Send + Sync {
    /// Sample the **ingest** subject only (never a query subject, §22 #7).
    async fn observe(&self) -> anyhow::Result<QueueObservation>;
}

/// Reconciles the running ingestion-worker fleet to a target count (§15/§16b).
#[async_trait]
pub trait Scaler: Send + Sync {
    /// Current replica/task count (for hysteresis input).
    async fn current(&self) -> anyhow::Result<usize>;
    /// Drive the fleet to exactly `target` replicas/tasks.
    async fn scale_to(&self, target: usize) -> anyhow::Result<()>;
}

/// NATS JetStream-backed [`QueueDepthSource`] (§15). Holds the stream/consumer
/// names for the ingest subject; construction deferred to `connect`.
pub struct JetStreamDepth {
    /// JetStream stream carrying ingest work units (spec §6.2). Ingest-only.
    ingest_stream: String,
}

impl JetStreamDepth {
    /// Connect to NATS and bind to the ingest stream's consumer (§15).
    pub async fn connect(_nats_url: &str, ingest_stream: String) -> anyhow::Result<Self> {
        // TODO: `async_nats::connect`, `jetstream::new`, look up the durable
        // consumer for the ingest subject and cache its handle. Map errors to
        // anyhow with context "connecting to NATS JetStream".
        Ok(Self { ingest_stream })
    }
}

#[async_trait]
impl QueueDepthSource for JetStreamDepth {
    async fn observe(&self) -> anyhow::Result<QueueObservation> {
        let _ = &self.ingest_stream;
        // TODO: read `consumer.info().await` → num_pending + num_ack_pending for
        // `depth`; derive `rate` from the delivered-count delta between polls.
        // ASSERT (debug) the subject is the ingest subject, never a query subject
        // (§22 #7).
        Ok(QueueObservation {
            depth: 0,
            rate: 0.0,
        })
    }
}

/// Cross-platform [`Scaler`] selected at runtime (Compose on Linux, tasks on
/// Windows) — mirrors the §16b.4 `ServiceManager` abstraction.
pub enum FleetScaler {
    /// `docker compose up -d --scale ingestion-worker=N` (Linux/unraid, §16b.3).
    DockerCompose {
        compose_file: String,
        service: String,
    },
    /// Adjust `diyragd` ingestion task count (Windows all-in-one, §16b.1).
    WindowsTaskCount,
}

#[async_trait]
impl Scaler for FleetScaler {
    async fn current(&self) -> anyhow::Result<usize> {
        match self {
            FleetScaler::DockerCompose { .. } => {
                // TODO: `docker compose ps --format json` count of running
                // ingestion-worker containers (tokio::process::Command).
                Ok(1)
            }
            FleetScaler::WindowsTaskCount => {
                // TODO: query diyragd for its current ingestion task count.
                Ok(1)
            }
        }
    }

    async fn scale_to(&self, target: usize) -> anyhow::Result<()> {
        match self {
            FleetScaler::DockerCompose {
                compose_file,
                service,
            } => {
                let _ = (compose_file, service, target);
                // TODO: spawn `docker compose -f {compose_file} up -d
                // --scale {service}={target}` via tokio::process::Command; check
                // exit status; never shell-interpolate untrusted input (§12.4).
                Ok(())
            }
            FleetScaler::WindowsTaskCount => {
                let _ = target;
                // TODO: signal diyragd to spawn/stop ingestion Tokio tasks to hit
                // `target` (§16b.1).
                Ok(())
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Container HEALTHCHECK form (`autoscaler healthcheck`): liveness only — the
    // autoscaler is a broker-queue controller and serves no HTTP /healthz (§16b).
    if diyrag_common::health::is_healthcheck_invocation() {
        std::process::exit(diyrag_common::health::liveness_ok());
    }

    // 1. Typed config (§0/§19).
    let config = AppConfig::load(Some("config/autoscaler.toml"))
        .context("loading autoscaler configuration")?;

    // 2. Logging (§13.1).
    logging::init(&config.observability).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    info!(service = %config.service_name, "starting autoscaler");

    // 3. Scaling policy config. DECISION: sourced under a dedicated config key
    //    (not yet on AppConfig, owned by another agent), so defaults are used and
    //    validated here rather than hardcoding magic numbers in the loop (§0).
    let scaling = ScalingConfig::default();
    scaling
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid scaling config: {e}"))?;

    // 4. Wire the ingest-only depth source + the platform scaler (§15/§16b.4).
    let depth_source = JetStreamDepth::connect(&config.nats.url, config.nats.stream.clone())
        .await
        .context("connecting JetStream depth source")?;

    // DECISION: choose the scaler by platform; Compose on Unix, task count on
    // Windows (mirrors §16b.4). Compose file/service come from config (no
    // hardcoded paths in production; placeholder default keeps the scaffold honest).
    let scaler: FleetScaler = if cfg!(windows) {
        FleetScaler::WindowsTaskCount
    } else {
        FleetScaler::DockerCompose {
            compose_file: "docker-compose.yml".to_owned(), // TODO: config key
            service: "ingestion-worker".to_owned(),
        }
    };

    // 5. Run the control loop until shutdown (graceful, §16b.2).
    let cancel = CancellationToken::new();
    let loop_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { control_loop(depth_source, scaler, scaling, cancel).await })
    };

    shutdown_signal().await;
    info!("shutdown signal received; stopping autoscaler");
    cancel.cancel();
    let _ = loop_handle.await;
    info!("autoscaler stopped");
    Ok(())
}

/// The poll → decide → reconcile loop (§15). Pure decision via [`desired_replicas`].
async fn control_loop(
    depth_source: impl QueueDepthSource,
    scaler: impl Scaler,
    scaling: ScalingConfig,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut ticker = tokio::time::interval(DEFAULT_POLL_INTERVAL);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                // Observe ingest backlog (never query subjects, §22 #7).
                let obs = match depth_source.observe().await {
                    Ok(o) => o,
                    Err(e) => { warn!(error = %e, "queue observation failed; holding fleet"); continue; }
                };
                let current = match scaler.current().await {
                    Ok(c) => c,
                    Err(e) => { warn!(error = %e, "reading current replica count failed; skipping"); continue; }
                };
                let target = desired_replicas(obs.depth, obs.rate, current, &scaling);
                if target != current {
                    info!(depth = obs.depth, rate = obs.rate, current, target, "scaling ingestion-worker fleet");
                    if let Err(e) = scaler.scale_to(target).await {
                        warn!(error = %e, target, "scale_to failed");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14/§16b.2).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct FakeDepth(u64);
    #[async_trait]
    impl QueueDepthSource for FakeDepth {
        async fn observe(&self) -> anyhow::Result<QueueObservation> {
            Ok(QueueObservation {
                depth: self.0,
                rate: 0.0,
            })
        }
    }

    struct FakeScaler {
        current: AtomicUsize,
        last_target: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Scaler for FakeScaler {
        async fn current(&self) -> anyhow::Result<usize> {
            Ok(self.current.load(Ordering::SeqCst))
        }
        async fn scale_to(&self, target: usize) -> anyhow::Result<()> {
            self.last_target.store(target, Ordering::SeqCst);
            self.current.store(target, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn loop_scales_then_stops_on_cancel() {
        let last = Arc::new(AtomicUsize::new(0));
        let scaler = FakeScaler {
            current: AtomicUsize::new(1),
            last_target: last.clone(),
        };
        // High backlog → policy wants more than 1; step cap (default 2) → 3.
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let h = tokio::spawn(async move {
            control_loop(FakeDepth(5000), scaler, ScalingConfig::default(), c2).await
        });
        // Let at least one tick fire, then cancel.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The interval's first tick is immediate, so a decision should have run.
        cancel.cancel();
        let _ = h.await;
        // From 1 with max_step 2 → target 3.
        assert_eq!(last.load(Ordering::SeqCst), 3);
    }
}
