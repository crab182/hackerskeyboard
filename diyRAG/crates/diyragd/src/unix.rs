//! Unix entry point: systemd `Restart=always` unit + Docker entrypoint
//! (MASTER_BUILD_SPEC.md §16b.3).
//!
//! There is no Service Control Manager on Linux/unraid; the OS analog of the
//! Windows-Service boot-autostart is:
//!   * **generic Linux** — a systemd unit (`deploy/systemd/diyragd.service`) with
//!     `Restart=always`, `WantedBy=multi-user.target`, started via
//!     `systemctl enable --now` (wrapped by `diyrag service install`);
//!   * **unraid / Docker** — `diyragd` is the container entrypoint and Docker's
//!     `restart: unless-stopped` plus unraid auto-starting Docker on array start
//!     brings the stack back after a reboot.
//!
//! In both cases the supervisor must drain gracefully when the init system asks
//! it to stop. systemd sends **SIGTERM** (then SIGKILL after `TimeoutStopSec`);
//! `docker stop` sends SIGTERM to PID 1; an interactive Ctrl-C sends **SIGINT**.
//! We translate either signal into a [`CancellationToken`] cancel and then await
//! the supervisor's drain — the exact same drain the Windows `Stop` handler uses
//! (§16b.2 / §14 service-level recovery).
//!
//! This module is `#[cfg(unix)]`-gated; `main` selects it on Unix targets.

#![cfg(unix)]

use anyhow::Result;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;

use crate::supervisor::{RunMode, Supervisor};

/// Run the supervisor under Unix signal handling.
///
/// Builds a multi-thread Tokio runtime, parses the run mode, starts the
/// [`Supervisor`], and races it against SIGTERM/SIGINT. On either signal it
/// fires the cancellation token and waits for the supervisor to finish draining
/// (ack/nak NATS, flush logs) before returning — giving the init system a clean
/// stop within its stop-timeout window.
pub fn run(mode_raw: String) -> Result<()> {
    // Minimal tracing init so the daemon logs to stdout/journald (JSON) even
    // before the per-service config is wired in. systemd/Docker capture stdout;
    // `journalctl -u diyragd` / `docker logs` then show structured lines.
    init_tracing();

    let mode = RunMode::parse(&mode_raw)?;
    tracing::info!(?mode, "diyragd starting (unix: systemd/Docker entrypoint)");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build Tokio runtime: {e}"))?;

    runtime.block_on(async move {
        let supervisor = Supervisor::new(mode);
        let cancel = CancellationToken::new();

        // Spawn the supervisor; it blocks until `cancel` fires, then drains.
        let supervisor_cancel = cancel.clone();
        let supervisor_task =
            tokio::spawn(async move { supervisor.run(supervisor_cancel).await });

        // Wait for a termination signal, then trigger graceful drain.
        wait_for_shutdown_signal().await?;
        tracing::info!("shutdown signal received; cancelling supervisor (graceful drain)");
        cancel.cancel();

        // Await the supervisor's drain. A join error means the task panicked,
        // which we surface as an error so the exit code is non-zero and the
        // init system's restart policy takes over (§14).
        match supervisor_task.await {
            Ok(result) => result,
            Err(join_err) => Err(anyhow::anyhow!("supervisor task panicked: {join_err}")),
        }
    })?;

    tracing::info!("diyragd stopped (drained)");
    Ok(())
}

/// Resolve when SIGTERM (systemd/`docker stop`) or SIGINT (Ctrl-C) arrives.
///
/// systemd's default `KillSignal` is SIGTERM and Docker forwards SIGTERM to the
/// entrypoint; we honor both plus SIGINT for interactive use. SIGHUP could later
/// be wired to config reload (TODO) but is not a stop signal here.
async fn wait_for_shutdown_signal() -> Result<()> {
    let mut sigterm =
        signal(SignalKind::terminate()).map_err(|e| anyhow::anyhow!("install SIGTERM handler: {e}"))?;
    let mut sigint =
        signal(SignalKind::interrupt()).map_err(|e| anyhow::anyhow!("install SIGINT handler: {e}"))?;

    tokio::select! {
        _ = sigterm.recv() => tracing::info!("received SIGTERM"),
        _ = sigint.recv() => tracing::info!("received SIGINT"),
    }
    Ok(())
}

/// Best-effort tracing initialization for the daemon.
///
/// DECISION: `diyragd` keeps a self-contained tracing setup rather than pulling
/// in `diyrag-common::logging`/`AppConfig`, because the supervisor wiring lands
/// before the typed config crate is consumed here. Honors `RUST_LOG`; falls back
/// to `info`. Swap for `diyrag_common::logging::init(&cfg.observability)` once
/// the per-node config file is read (§13.1). `try_init` is idempotent-safe so a
/// double init (e.g. in tests) does not panic.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // JSON logs to stdout for journald/`docker logs` (§13.1). Ignore the error:
    // a global subscriber may already be installed (tests / re-entry).
    let _ = fmt().json().with_env_filter(filter).try_init();
}
