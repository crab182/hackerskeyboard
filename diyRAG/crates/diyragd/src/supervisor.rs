//! Cross-platform process supervisor (MASTER_BUILD_SPEC.md §16b.1).
//!
//! The [`Supervisor`] owns the run-mode orchestration that is identical on
//! Windows (driven by [`crate::windows_service`]) and Unix (driven by
//! [`crate::unix`]). It is **pure async, cancellable, and restart-on-failure
//! with backoff** so the *same* logic backs the Windows Service auto-start, the
//! systemd `Restart=always` unit, and the Docker entrypoint.
//!
//! Per [`RunMode`]:
//! * [`RunMode::AllInOne`] — single-node homelab mode: start `core-api`,
//!   `retrieval`, `ingestion-worker`(s), `mcp-server`, `sync-agent`, and the
//!   Rust-native inference backend **as Tokio tasks in one process** (§16b.1).
//! * [`RunMode::Agent`] — orchestrate an external service set (Docker Compose on
//!   unraid/Linux) and report health (§16b.1 / §16b.3).
//! * [`RunMode::Service`] — run exactly one named service (used by Compose
//!   replicas / `diyragd service --mode service:<name>`).
//!
//! Cancellation contract: every long-running task selects on the supplied
//! [`CancellationToken`]; when it fires, tasks drain in-flight work units,
//! ack/nak NATS, flush logs, and return. [`Supervisor::run`] returns only once
//! every child has drained — this is the "graceful drain" the SCM `Stop`
//! handler and the Unix SIGTERM handler both depend on.

use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

/// Which set of services this supervisor instance is responsible for (§16b.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    /// All Rust services as Tokio tasks in one process (default Windows box).
    #[default]
    AllInOne,
    /// Orchestrate the external Docker Compose stack (unraid/Linux).
    Agent,
    /// Run exactly one named service (Compose replica / sharded deploy).
    Service(String),
}

impl RunMode {
    /// Parse the `--mode` flag value used by `diyragd service --mode <…>` and
    /// the `sc.exe binPath=` argument (§16b.2). Unknown values are an error so a
    /// typo in a service registration fails loudly rather than silently running
    /// the wrong topology.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "all-in-one" => Ok(Self::AllInOne),
            "agent" => Ok(Self::Agent),
            other => match other.strip_prefix("service:") {
                Some(name) if !name.is_empty() => Ok(Self::Service(name.to_string())),
                _ => anyhow::bail!(
                    "unknown run mode '{other}': expected 'all-in-one', 'agent', or 'service:<name>'"
                ),
            },
        }
    }
}

/// Restart-on-failure policy with exponential backoff (§14 taxonomy: TRANSIENT
/// failures retry; the supervisor never lets one crashed task take the process
/// down — `panic = "abort"` in release means a panic exits the process and the
/// SCM/systemd/Docker restart policy brings it back, §14 service-level recovery).
#[derive(Clone, Debug)]
pub struct BackoffPolicy {
    pub base: Duration,
    pub max: Duration,
    pub max_restarts: u32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        // T = 2^(n-1) × base, capped at `max` (§14).
        Self {
            base: Duration::from_millis(500),
            max: Duration::from_secs(30),
            max_restarts: 10,
        }
    }
}

impl BackoffPolicy {
    /// Delay before the `n`-th restart (1-indexed): `2^(n-1) × base`, capped.
    pub fn delay_for(&self, restart: u32) -> Duration {
        let factor = 1u64.checked_shl(restart.saturating_sub(1)).unwrap_or(u64::MAX);
        self.base
            .saturating_mul(factor.min(u32::MAX as u64) as u32)
            .min(self.max)
    }
}

/// The supervisor: holds the run-mode and a cancellation token shared with the
/// platform layer (Windows Service control handler / Unix signal handler).
pub struct Supervisor {
    mode: RunMode,
    backoff: BackoffPolicy,
}

impl Supervisor {
    pub fn new(mode: RunMode) -> Self {
        Self {
            mode,
            backoff: BackoffPolicy::default(),
        }
    }

    pub fn with_backoff(mut self, backoff: BackoffPolicy) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn mode(&self) -> &RunMode {
        &self.mode
    }

    /// Boot the configured services and block until `cancel` fires, then drain.
    ///
    /// This is the single entry point both platform layers call:
    /// * `windows_service::service_main` calls it on the Tokio runtime and fires
    ///   `cancel` from the SCM `Stop`/`Shutdown` control handler.
    /// * `unix::run` calls it and fires `cancel` on SIGTERM/SIGINT.
    pub async fn run(&self, cancel: CancellationToken) -> Result<()> {
        tracing::info!(mode = ?self.mode, "supervisor starting");

        match &self.mode {
            RunMode::AllInOne => self.run_all_in_one(cancel.clone()).await?,
            RunMode::Agent => self.run_agent(cancel.clone()).await?,
            RunMode::Service(name) => self.run_single_service(name, cancel.clone()).await?,
        }

        tracing::info!("supervisor stopped (all children drained)");
        Ok(())
    }

    /// All-in-one: spawn each Rust service as a supervised Tokio task (§16b.1).
    async fn run_all_in_one(&self, cancel: CancellationToken) -> Result<()> {
        // The fixed service set for a single homelab node (spec §2 / §16b.1).
        const SERVICES: &[&str] = &[
            "core-api",
            "retrieval",
            "ingestion-worker",
            "mcp-server",
            "sync-agent",
            "inference-backend", // Rust-native ort/mistral.rs (§16)
        ];

        let mut tasks = Vec::with_capacity(SERVICES.len());
        for &name in SERVICES {
            let child_cancel = cancel.clone();
            let backoff = self.backoff.clone();
            let name = name.to_string();
            tasks.push(tokio::spawn(async move {
                supervise_task(&name, child_cancel, backoff).await
            }));
        }

        // Block until the supervisor is cancelled, then await graceful drain of
        // every supervised task (so NATS acks/naks and log flushes complete).
        cancel.cancelled().await;
        tracing::info!("cancellation received; draining all-in-one services");
        for task in tasks {
            // A child task returning Err is logged inside `supervise_task`; join
            // errors (task panicked) are surfaced but must not abort the drain.
            if let Err(join_err) = task.await {
                tracing::error!(error = %join_err, "service task join failed during drain");
            }
        }
        Ok(())
    }

    /// Agent mode: orchestrate the external Docker Compose stack (§16b.3).
    async fn run_agent(&self, cancel: CancellationToken) -> Result<()> {
        // TODO: bring the stack up via `docker compose up -d` (reuse the
        // DockerCompose ServiceManager from diyrag-cli) and poll `docker compose
        // ps` / per-service /healthz for health reporting. On cancel, leave the
        // stack running (Docker `restart: unless-stopped` owns its lifecycle) but
        // flush our own health-reporter state. See spec §16b.3.
        tracing::info!("agent mode: orchestrating external Docker Compose stack (health reporting)");
        cancel.cancelled().await;
        tracing::info!("cancellation received; stopping agent health reporter");
        Ok(())
    }

    /// Service mode: run exactly one named service in this process (§16b.1).
    async fn run_single_service(&self, name: &str, cancel: CancellationToken) -> Result<()> {
        supervise_task(name, cancel, self.backoff.clone()).await
    }
}

/// Supervise a single logical service: (re)start its async entry point with
/// exponential backoff until cancellation, draining cleanly on cancel.
async fn supervise_task(
    name: &str,
    cancel: CancellationToken,
    backoff: BackoffPolicy,
) -> Result<()> {
    let mut restarts: u32 = 0;
    loop {
        if cancel.is_cancelled() {
            tracing::info!(service = name, "not (re)starting; supervisor cancelled");
            return Ok(());
        }

        tracing::info!(service = name, restarts, "starting service task");
        let result = run_service_entrypoint(name, cancel.clone()).await;

        match result {
            // Clean shutdown in response to cancellation — stop supervising.
            Ok(()) if cancel.is_cancelled() => {
                tracing::info!(service = name, "service drained after cancellation");
                return Ok(());
            }
            // The entry point returned without cancellation: treat as a crash to
            // restart (a healthy service runs until cancelled).
            Ok(()) => {
                tracing::warn!(service = name, "service exited unexpectedly; will restart");
            }
            Err(err) => {
                tracing::error!(service = name, error = %err, "service task failed; will restart");
            }
        }

        restarts += 1;
        if restarts > backoff.max_restarts {
            anyhow::bail!("service '{name}' exceeded max restarts ({})", backoff.max_restarts);
        }

        let delay = backoff.delay_for(restarts);
        tracing::warn!(service = name, restarts, ?delay, "backing off before restart");
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => {
                tracing::info!(service = name, "cancelled during backoff");
                return Ok(());
            }
        }
    }
}

/// The actual per-service async body. Selects on `cancel` so the service drains
/// in-flight work and returns promptly on stop.
async fn run_service_entrypoint(name: &str, cancel: CancellationToken) -> Result<()> {
    // TODO: dispatch to the real service entry points. In `all-in-one` these are
    // library `serve(cancel)` functions exported by the sibling crates
    // (diyrag-core-api, diyrag-retrieval, diyrag-ingestion-worker,
    // diyrag-mcp-server, diyrag-sync-agent) plus the Rust-native inference
    // backend. Each must: bind/connect with config from `common::config`,
    // register /healthz + /readyz, and run until `cancel` fires, then drain
    // (ack/nak NATS, flush tracing). This stub just parks until cancelled so the
    // supervisor wiring is exercisable end-to-end before the services land.
    tracing::debug!(service = name, "service entrypoint running (stub: parks until cancel)");
    cancel.cancelled().await;
    tracing::info!(service = name, "service entrypoint draining");
    // TODO: flush per-service buffers / await in-flight work-unit completion.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_mode_parses_known_values() {
        assert_eq!(RunMode::parse("all-in-one").unwrap(), RunMode::AllInOne);
        assert_eq!(RunMode::parse("agent").unwrap(), RunMode::Agent);
        assert_eq!(
            RunMode::parse("service:core-api").unwrap(),
            RunMode::Service("core-api".to_string())
        );
    }

    #[test]
    fn run_mode_rejects_garbage() {
        assert!(RunMode::parse("nope").is_err());
        assert!(RunMode::parse("service:").is_err());
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let p = BackoffPolicy {
            base: Duration::from_millis(100),
            max: Duration::from_secs(1),
            max_restarts: 10,
        };
        assert_eq!(p.delay_for(1), Duration::from_millis(100)); // 2^0 × base
        assert_eq!(p.delay_for(2), Duration::from_millis(200)); // 2^1 × base
        assert_eq!(p.delay_for(3), Duration::from_millis(400)); // 2^2 × base
        assert_eq!(p.delay_for(20), Duration::from_secs(1)); // capped at max
    }

    #[tokio::test]
    async fn supervisor_drains_on_cancel() {
        let sup = Supervisor::new(RunMode::Service("core-api".to_string()));
        let cancel = CancellationToken::new();
        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move { sup.run(cancel).await })
        };
        // Let it boot, then cancel and confirm it returns Ok (drained).
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("supervisor did not drain in time")
            .expect("task panicked");
        assert!(res.is_ok());
    }
}
