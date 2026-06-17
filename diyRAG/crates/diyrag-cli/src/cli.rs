//! `diyrag` command tree (MASTER_BUILD_SPEC.md §16b.1).
//!
//! The clap-derive types live here so both [`crate::main`] and
//! [`crate::service_manager`] can share `ManagerKind`/`ServiceConfig` without a
//! cycle. The command surface is identical on Windows, unraid, and generic
//! Linux (acceptance #9).

use clap::{Args, Parser, Subcommand, ValueEnum};

/// `diyrag` — the diyRAG terminal control plane (§16b.1).
#[derive(Debug, Parser)]
#[command(
    name = "diyrag",
    about = "diyRAG control CLI: service management, node ops, ingest, batch, query, config",
    version,
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the OS service (Windows SCM / systemd / Docker Compose) — §16b.2/3.
    #[command(subcommand)]
    Service(ServiceCommand),

    /// Node + LAN-sync operations (§9 / §16b.1).
    #[command(subcommand)]
    Node(NodeCommand),

    /// Ingest a file, folder, or registered root via the REST API (§6 / §16b.1).
    Ingest(IngestArgs),

    /// Batch operations (archives / large trees) (§6.7).
    #[command(subcommand)]
    Batch(BatchCommand),

    /// Run a search or grounded-answer query (§7 / §16b.1).
    Query(QueryArgs),

    /// Typed configuration (12-factor; env overrides) (§16b.1).
    #[command(subcommand)]
    Config(ConfigCommand),
}

// --- service ---------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Register the service so it auto-starts on boot (§16b.2/3).
    Install(InstallArgs),
    /// Remove the service registration (data retained).
    Uninstall(ManagerOverride),
    /// Start the service now.
    Start(ManagerOverride),
    /// Stop the service (graceful drain).
    Stop(ManagerOverride),
    /// Restart the service.
    Restart(ManagerOverride),
    /// Report service status.
    Status(ManagerOverride),
}

/// Which service-manager backend to use; default chosen by `cfg!(windows)`
/// (§16b.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ManagerKind {
    /// Windows Service Control Manager (`windows-service`).
    Scm,
    /// Linux systemd (`systemctl`).
    Systemd,
    /// Docker Compose (the unraid path).
    Docker,
}

/// Shared `--manager` override carried by the no-arg lifecycle subcommands.
#[derive(Debug, Args)]
pub struct ManagerOverride {
    /// Override the auto-selected service manager (default: SCM on Windows,
    /// systemd on Linux; unraid uses `docker`).
    #[arg(long, value_enum)]
    pub manager: Option<ManagerKind>,

    /// Compose file path (only used with `--manager docker`).
    #[arg(long, default_value = "docker-compose.yml")]
    pub compose_file: String,
}

/// Arguments for `diyrag service install` (§16b.2).
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Run mode baked into the service registration: `all-in-one` | `agent` |
    /// `service:<name>` (§16b.1).
    #[arg(long, default_value = "all-in-one")]
    pub mode: String,

    /// Register for boot auto-start (§16b.2). Default true; the flag exists so a
    /// manual-start install is expressible.
    #[arg(long, default_value_t = true)]
    pub auto_start: bool,

    /// Low-privilege service account, e.g. `NT SERVICE\diyRAG` (Windows; §12.8).
    /// On systemd this maps to `User=`. Omit for the platform default.
    #[arg(long)]
    pub account: Option<String>,

    /// Path to the `diyragd` binary the service will launch. Defaults to the
    /// sibling `diyragd`/`diyragd.exe` next to this CLI.
    #[arg(long)]
    pub binary_path: Option<String>,

    #[command(flatten)]
    pub manager: ManagerOverride,
}

/// Resolved, backend-agnostic install parameters handed to [`crate::service_manager`].
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub mode: String,
    pub auto_start: bool,
    pub account: Option<String>,
    /// Absolute path to the `diyragd` executable the service launches.
    pub binary_path: std::path::PathBuf,
}

impl ServiceConfig {
    /// Build from CLI args, resolving the `diyragd` binary path if not given.
    pub fn from_args(args: &InstallArgs) -> Self {
        let binary_path = args
            .binary_path
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(default_daemon_path);
        Self {
            mode: args.mode.clone(),
            auto_start: args.auto_start,
            account: args.account.clone(),
            binary_path,
        }
    }
}

/// Best-effort default path to `diyragd`: the `diyragd[.exe]` sibling of this
/// CLI executable (they ship together in the MSI / package, §16b.2).
fn default_daemon_path() -> std::path::PathBuf {
    let exe_name = if cfg!(windows) { "diyragd.exe" } else { "diyragd" };
    if let Ok(self_exe) = std::env::current_exe() {
        if let Some(dir) = self_exe.parent() {
            return dir.join(exe_name);
        }
    }
    std::path::PathBuf::from(exe_name)
}

// --- node ------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    /// This node's health/runtime status (§11.2 `/admin/runtime`).
    Status,
    /// Known LAN peers and their sync state (§9).
    Peers,
    /// Take a Qdrant snapshot (the unit of vector replication, §9).
    Snapshot,
    /// Restore from a snapshot.
    Restore(RestoreArgs),
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Snapshot identifier / path to restore from.
    pub snapshot: String,
}

// --- ingest ----------------------------------------------------------------

#[derive(Debug, Args)]
pub struct IngestArgs {
    /// File, folder, or registered root path to ingest.
    pub path: String,
    /// Register the path as a watched root (debounced re-ingest on change, §6.1).
    #[arg(long)]
    pub watch: bool,
}

// --- batch -----------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum BatchCommand {
    /// Submit an archive (ZIP/TAR) or path list for batch ingestion (§6.7).
    Submit(BatchSubmitArgs),
}

#[derive(Debug, Args)]
pub struct BatchSubmitArgs {
    /// Path to the archive to submit.
    pub archive: String,
}

// --- query -----------------------------------------------------------------

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// The query string.
    pub query: String,
    /// Return a grounded answer with citations instead of raw search hits (§7.2).
    #[arg(long)]
    pub answer: bool,
    /// Number of results / chunks to retrieve.
    #[arg(long, default_value_t = 10)]
    pub k: usize,
}

// --- config ----------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show the effective (env-overridden) configuration.
    Show,
    /// Set a configuration key.
    Set(ConfigSetArgs),
}

#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    /// Config key (dotted path, e.g. `api.base_url`).
    pub key: String,
    /// New value.
    pub value: String,
}
