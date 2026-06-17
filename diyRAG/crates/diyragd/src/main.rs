#![forbid(unsafe_code)]
//! `diyragd` — the diyRAG supervisor daemon binary (MASTER_BUILD_SPEC.md §16b).
//!
//! One binary, three entry paths that all converge on the cross-platform
//! [`supervisor::Supervisor`]:
//!
//! 1. **Windows Service** — when the Service Control Manager launches
//!    `diyragd.exe service --mode <…>` on every boot (because the service was
//!    registered with `StartType::AutoStart` by `diyrag service install`), the
//!    `service` subcommand hands control to [`windows_service::run`], which
//!    connects to the SCM dispatcher. This is what makes the node "come back
//!    after a reboot" (acceptance #9).
//! 2. **systemd / Docker (Unix)** — the same `service` subcommand (or the
//!    default interactive run) calls [`unix::run`], a Tokio `main` that wires
//!    SIGTERM/SIGINT to the supervisor's [`CancellationToken`] so systemd
//!    `Restart=always` and the Docker entrypoint drain gracefully (§16b.3).
//! 3. **Interactive** — running `diyragd` (no subcommand) or `diyragd run`
//!    starts the supervisor in the foreground for local development; Ctrl-C
//!    drains it. On Windows this is the non-SCM path (e.g. a developer console).
//!
//! Errors use `anyhow` at the binary boundary (§19). No `unsafe` (§12.8).

mod supervisor;

#[cfg(unix)]
mod unix;

#[cfg(windows)]
mod windows_service;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// `diyragd` — supervisor daemon + native service host (spec §16b.1).
#[derive(Debug, Parser)]
#[command(
    name = "diyragd",
    about = "diyRAG supervisor daemon (Windows Service / systemd / Docker entrypoint)",
    version,
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Run mode for the default (interactive) run when no subcommand is given.
    /// One of `all-in-one`, `agent`, `service:<name>` (spec §16b.1).
    #[arg(long, global = true, default_value = "all-in-one")]
    mode: String,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// The entry point the OS service manager invokes.
    ///
    /// * Windows: this is what `sc.exe create diyRAG binPath= "…\diyragd.exe
    ///   service --mode all-in-one"` runs on every boot; it connects to the SCM
    ///   (spec §16b.2).
    /// * Unix: equivalent to the foreground run, but named so the systemd
    ///   `ExecStart=` and the Docker `CMD` are explicit and symmetric with
    ///   Windows.
    Service {
        /// Run mode: `all-in-one` | `agent` | `service:<name>` (spec §16b.1).
        #[arg(long, default_value = "all-in-one")]
        mode: String,
    },

    /// Run the supervisor in the foreground (local dev / non-SCM console).
    Run {
        /// Run mode: `all-in-one` | `agent` | `service:<name>` (spec §16b.1).
        #[arg(long, default_value = "all-in-one")]
        mode: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // Service-manager entry point.
        Some(Command::Service { mode }) => dispatch_service(mode),
        // Explicit foreground run.
        Some(Command::Run { mode }) => run_interactive(mode),
        // No subcommand: default interactive run using the global `--mode`.
        None => run_interactive(cli.mode),
    }
}

/// Dispatch to the platform's service-manager entry point.
///
/// On Windows this connects to the SCM (blocking until the service stops). On
/// Unix there is no SCM, so we run the same supervisor loop as the interactive
/// path — systemd/Docker own the process lifecycle and signal us with SIGTERM.
fn dispatch_service(mode: String) -> Result<()> {
    #[cfg(windows)]
    {
        // Hands off to the SCM dispatcher; returns only once the service stops.
        // Tracing/Event Log init happens inside `windows_service::run`.
        windows_service::run(mode)
    }

    #[cfg(unix)]
    {
        // No SCM on Unix; systemd `Restart=always` / Docker `restart:` is the
        // analog. Run the supervisor with signal-driven cancellation (§16b.3).
        unix::run(mode)
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = mode;
        anyhow::bail!("unsupported platform: no service manager integration");
    }
}

/// Foreground/interactive run. Shared by the `run` subcommand and the bare
/// `diyragd` invocation. On Unix it reuses the signal-aware runner so Ctrl-C
/// (SIGINT) drains cleanly; on Windows it runs a Tokio runtime with a Ctrl-C
/// handler for developer consoles.
fn run_interactive(mode: String) -> Result<()> {
    #[cfg(unix)]
    {
        unix::run(mode)
    }

    #[cfg(windows)]
    {
        windows_service::run_interactive(mode)
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = mode;
        anyhow::bail!("unsupported platform");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::RunMode;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // Catches arg-graph mistakes (duplicate flags, bad defaults) at test time.
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_service_subcommand_with_mode() {
        let cli = Cli::parse_from(["diyragd", "service", "--mode", "agent"]);
        match cli.command {
            Some(Command::Service { mode }) => assert_eq!(mode, "agent"),
            other => panic!("expected service subcommand, got {other:?}"),
        }
    }

    #[test]
    fn bare_invocation_defaults_to_all_in_one() {
        let cli = Cli::parse_from(["diyragd"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.mode, "all-in-one");
        // And the mode string is accepted by the supervisor parser.
        assert_eq!(RunMode::parse(&cli.mode).unwrap(), RunMode::AllInOne);
    }
}
