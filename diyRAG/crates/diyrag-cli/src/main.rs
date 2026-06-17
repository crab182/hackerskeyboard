#![forbid(unsafe_code)]
//! `diyrag` — the diyRAG terminal control plane (MASTER_BUILD_SPEC.md §16b.1).
//!
//! One CLI, identical on Windows, unraid, and generic Linux (acceptance #9):
//!
//! * `diyrag service install|uninstall|start|stop|restart|status` — manage the
//!   OS service via the [`service_manager::ServiceManager`] abstraction
//!   (Windows SCM / systemd / Docker Compose), selected by `cfg!(windows)` with
//!   a `--manager` override (§16b.4).
//! * `diyrag node status|peers|snapshot|restore` — node + LAN-sync ops (§9).
//! * `diyrag ingest <path|root> [--watch]`, `diyrag batch submit <archive>`,
//!   `diyrag query "<q>" [--answer]` — drive the REST API headlessly (§7 / §6).
//! * `diyrag config show|set` — typed, env-overridable config (§16b.1).
//!
//! Errors use `anyhow` at the binary boundary (§19). No `unsafe` (§12.8). No
//! secrets: the API key is read from the environment, never a flag (§12.2).

mod api;
mod cli;
mod service_manager;

use anyhow::Result;
use clap::Parser;

use crate::api::ApiClient;
use crate::cli::{
    BatchCommand, Cli, Command, ConfigCommand, ManagerOverride, NodeCommand, ServiceCommand,
    ServiceConfig,
};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Service(cmd) => run_service(cmd),
        Command::Node(cmd) => run_node(cmd).await,
        Command::Ingest(args) => {
            ApiClient::from_env()?.ingest(&args.path, args.watch).await
        }
        Command::Batch(BatchCommand::Submit(args)) => {
            ApiClient::from_env()?.batch_submit(&args.archive).await
        }
        Command::Query(args) => {
            ApiClient::from_env()?
                .query(&args.query, args.k, args.answer)
                .await
        }
        Command::Config(cmd) => run_config(cmd),
    }
}

/// Dispatch the `service …` subcommands to the resolved [`ServiceManager`].
///
/// Synchronous: the backends shell out to the OS service manager / SCM. The
/// `--manager` override and (for Docker) `--compose-file` come from the shared
/// [`ManagerOverride`] flatten group.
fn run_service(cmd: ServiceCommand) -> Result<()> {
    match cmd {
        ServiceCommand::Install(args) => {
            let mgr = build_manager(&args.manager)?;
            let cfg = ServiceConfig::from_args(&args);
            tracing::info!(
                manager = mgr.name(),
                mode = %cfg.mode,
                auto_start = cfg.auto_start,
                "installing service"
            );
            mgr.install(&cfg)
        }
        ServiceCommand::Uninstall(o) => build_manager(&o)?.uninstall(),
        ServiceCommand::Start(o) => build_manager(&o)?.start(),
        ServiceCommand::Stop(o) => build_manager(&o)?.stop(),
        ServiceCommand::Restart(o) => build_manager(&o)?.restart(),
        ServiceCommand::Status(o) => build_manager(&o)?.status(),
    }
}

/// Select + configure the service manager from a [`ManagerOverride`].
fn build_manager(o: &ManagerOverride) -> Result<Box<dyn service_manager::ServiceManager>> {
    let mgr = service_manager::select(o.manager)?;
    // The Docker impl needs the compose-file path; `select` builds it with the
    // default, so for the docker backend we rebuild with the override applied.
    if mgr.name() == "docker-compose" {
        return Ok(Box::new(
            service_manager::DockerCompose::new().with_compose_file(o.compose_file.clone()),
        ));
    }
    Ok(mgr)
}

/// Dispatch the `node …` subcommands to the REST client (§9).
async fn run_node(cmd: NodeCommand) -> Result<()> {
    let client = ApiClient::from_env()?;
    match cmd {
        NodeCommand::Status => client.node_status().await,
        NodeCommand::Peers => client.node_peers().await,
        NodeCommand::Snapshot => client.node_snapshot().await,
        NodeCommand::Restore(args) => client.node_restore(&args.snapshot).await,
    }
}

/// Dispatch the `config …` subcommands (§16b.1).
fn run_config(cmd: ConfigCommand) -> Result<()> {
    match cmd {
        ConfigCommand::Show => {
            // TODO: load the typed config (diyrag_common::config::AppConfig once
            // this crate depends on common) and print the effective, env-merged
            // view with secrets redacted (§12.2).
            tracing::info!("TODO: render effective configuration (secrets redacted)");
            Ok(())
        }
        ConfigCommand::Set(args) => {
            // TODO: persist the key/value to the on-disk config under
            // %ProgramData%\diyRAG (Windows) / appdata (unraid), validated
            // against the typed schema; reject unknown keys loudly.
            tracing::info!(key = %args.key, "TODO: persist config key (value not logged)");
            Ok(())
        }
    }
}

/// Best-effort tracing init for the CLI. Honors `RUST_LOG`; defaults to `warn`
/// so headless scripts get clean stdout (the command output) without log noise.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // Logs to stderr so they don't corrupt machine-readable stdout output.
    let _ = fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_query_with_answer_flag() {
        let cli = Cli::parse_from(["diyrag", "query", "what is x?", "--answer", "--k", "5"]);
        match cli.command {
            Command::Query(args) => {
                assert_eq!(args.query, "what is x?");
                assert!(args.answer);
                assert_eq!(args.k, 5);
            }
            other => panic!("expected query, got {other:?}"),
        }
    }

    #[test]
    fn parses_service_install_defaults() {
        let cli = Cli::parse_from(["diyrag", "service", "install"]);
        match cli.command {
            Command::Service(ServiceCommand::Install(args)) => {
                assert_eq!(args.mode, "all-in-one");
                assert!(args.auto_start);
            }
            other => panic!("expected service install, got {other:?}"),
        }
    }
}
