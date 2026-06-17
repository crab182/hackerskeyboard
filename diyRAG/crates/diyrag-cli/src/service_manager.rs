//! Cross-platform service-management abstraction (MASTER_BUILD_SPEC.md §16b.4).
//!
//! A single [`ServiceManager`] trait with three implementations keeps the
//! `diyrag service …` CLI surface identical across Windows, unraid, and generic
//! Linux (acceptance #9):
//!
//! | Impl | Platform | Backend |
//! |---|---|---|
//! | [`WindowsScm`] | Windows | Service Control Manager via the `windows-service` crate. |
//! | [`Systemd`]    | Linux   | `systemctl enable --now` / `stop` / `status` / `disable`. |
//! | [`DockerCompose`] | unraid / any Docker host | `docker compose up -d` / `down` / `ps`. |
//!
//! [`select`] picks the default impl by `cfg!(windows)`, with a `--manager`
//! override (`scm` | `systemd` | `docker`) so an unraid box can force the Docker
//! path even though it is Linux, and a Windows box with Docker Desktop can target
//! Compose for `agent` mode.

use anyhow::{Context, Result};

use crate::cli::{ManagerKind, ManagerOverride, ServiceConfig};

/// Lifecycle operations every backend supports (§16b.1).
///
/// Implementations shell out to (or call into) the OS service manager. They are
/// intentionally synchronous-friendly but return `Result` so the CLI can render
/// a structured error envelope. High-risk ops (`install`/`uninstall`) are
/// deny-by-default elsewhere (RBAC) and require local admin/root.
pub trait ServiceManager {
    /// Human-readable name of the backend, for logs / `--manager` echo.
    fn name(&self) -> &'static str;

    /// Register the service so it **auto-starts on boot** (§16b.2 / §16b.3).
    fn install(&self, cfg: &ServiceConfig) -> Result<()>;

    /// Remove the service registration (logical; data under ProgramData/appdata
    /// is retained — §6.6 retention spirit).
    fn uninstall(&self) -> Result<()>;

    /// Start the service now.
    fn start(&self) -> Result<()>;

    /// Stop the service (graceful drain handled by `diyragd`, §16b.2).
    fn stop(&self) -> Result<()>;

    /// Restart = stop then start (default impl; backends may override for an
    /// atomic restart, e.g. `systemctl restart`).
    fn restart(&self) -> Result<()> {
        self.stop()?;
        self.start()
    }

    /// Report the current status (running/stopped + detail) to stdout.
    fn status(&self) -> Result<()>;
}

/// Choose the [`ServiceManager`] impl from a CLI [`ManagerOverride`]: explicit
/// `--manager`, else the platform default (`cfg!(windows)` ⇒ SCM, else systemd).
/// The `--compose-file` value flows into the Docker backend (§16b.4).
pub fn select(over: &ManagerOverride) -> Result<Box<dyn ServiceManager>> {
    let kind = over.manager.unwrap_or_else(default_manager_kind);
    match kind {
        ManagerKind::Scm => {
            #[cfg(windows)]
            {
                Ok(Box::new(WindowsScm::new()))
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!(
                    "--manager scm is only available on Windows; use systemd or docker on this host"
                )
            }
        }
        ManagerKind::Systemd => {
            #[cfg(unix)]
            {
                Ok(Box::new(Systemd::new()))
            }
            #[cfg(not(unix))]
            {
                anyhow::bail!(
                    "--manager systemd is only available on Linux; use scm or docker on this host"
                )
            }
        }
        ManagerKind::Docker => Ok(Box::new(
            DockerCompose::new().with_compose_file(over.compose_file.clone()),
        )),
    }
}

/// Platform default backend kind (used when `--manager` is not given).
fn default_manager_kind() -> ManagerKind {
    if cfg!(windows) {
        ManagerKind::Scm
    } else {
        // DECISION: default to systemd on Linux. unraid users pass
        // `--manager docker` (documented in the README), because unraid's
        // first-class app model is Docker, not systemd units (§16b.3).
        ManagerKind::Systemd
    }
}

// ---------------------------------------------------------------------------
// Windows: Service Control Manager (§16b.2)
// ---------------------------------------------------------------------------

/// Manage the diyRAG service through the Windows Service Control Manager using
/// the `windows-service` crate (§16b.2).
///
/// ## `sc.exe` equivalents (reference / fallback)
/// The crate calls mirror these `sc.exe` commands; we use the crate, not
/// `sc.exe`, so install is a typed, testable operation:
/// ```text
/// sc.exe create diyRAG binPath= "C:\Program Files\diyRAG\diyragd.exe service --mode all-in-one" start= auto type= own obj= "NT SERVICE\diyRAG"
/// sc.exe failure diyRAG reset= 86400 actions= restart/5000/restart/10000/restart/30000
/// sc.exe start diyRAG
/// sc.exe stop diyRAG
/// sc.exe delete diyRAG
/// sc.exe query diyRAG
/// ```
#[cfg(windows)]
pub struct WindowsScm {
    service_name: std::ffi::OsString,
}

#[cfg(windows)]
impl WindowsScm {
    /// SCM service name — MUST match `diyragd`'s `SERVICE_NAME` and the docs.
    const SERVICE_NAME: &'static str = "diyRAG";
    const DISPLAY_NAME: &'static str = "diyRAG supervisor";

    pub fn new() -> Self {
        Self {
            service_name: std::ffi::OsString::from(Self::SERVICE_NAME),
        }
    }

    /// Open the SCM with the access level needed for an operation.
    fn open_manager(
        access: windows_service::service_manager::ServiceManagerAccess,
    ) -> Result<windows_service::service_manager::ServiceManager> {
        use windows_service::service_manager::ServiceManager as Scm;
        // `None` target = the local machine, `None` database = the active SCM db.
        Scm::local_computer(None::<&str>, access)
            .context("opening the Windows Service Control Manager (need admin rights)")
    }
}

#[cfg(windows)]
impl Default for WindowsScm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl ServiceManager for WindowsScm {
    fn name(&self) -> &'static str {
        "windows-scm"
    }

    fn install(&self, cfg: &ServiceConfig) -> Result<()> {
        use std::time::Duration;
        use windows_service::service::{
            ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
        };
        use windows_service::service_manager::ServiceManagerAccess;

        // CREATE_SERVICE access to register; CHANGE_CONFIG to set recovery.
        let scm = Self::open_manager(
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;

        // The SCM launches: `diyragd.exe service --mode <mode>` — the boot
        // autostart entry point (§16b.2). `launch_arguments` are passed to the
        // service on start; we also bake them so a manual `sc start` works.
        let info = ServiceInfo {
            name: self.service_name.clone(),
            display_name: std::ffi::OsString::from(Self::DISPLAY_NAME),
            // OWN_PROCESS: the service runs in its own diyragd.exe (§16b.2).
            service_type: ServiceType::OWN_PROCESS,
            // AutoStart => starts on every device restart / boot (§16b.2).
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: cfg.binary_path.clone(),
            launch_arguments: vec![
                std::ffi::OsString::from("service"),
                std::ffi::OsString::from("--mode"),
                std::ffi::OsString::from(cfg.mode.clone()),
            ],
            dependencies: vec![],
            // Low-privilege account (e.g. `NT SERVICE\diyRAG`) — never
            // LocalSystem unless GPU access requires it (§12.8 / §16b.2). `None`
            // means LocalSystem; the CLI passes `Some(account)` from --account.
            account_name: cfg.account.as_deref().map(std::ffi::OsString::from),
            // Password for a domain/managed account; virtual accounts need none.
            account_password: None,
        };

        // Right to set the description + failure actions after create.
        let service = scm
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .context("creating the diyRAG service (sc.exe create … start= auto)")?;

        service
            .set_description("diyRAG self-hosted RAG supervisor (auto-start on boot).")
            .context("setting service description")?;

        // Recovery / failure actions — restart on 1st/2nd/3rd failure with
        // backoff, satisfying §14 service-level recovery
        // (equivalent: `sc.exe failure diyRAG reset= 86400 actions= restart/5000/restart/10000/restart/30000`).
        {
            use windows_service::service::{
                ServiceAction, ServiceActionType, ServiceFailureActions, ServiceFailureResetPeriod,
            };
            let actions = ServiceFailureActions {
                // Reset the failure counter after 1 day of healthy uptime.
                reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
                reboot_msg: None,
                command: None,
                actions: Some(vec![
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(5),
                    },
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(10),
                    },
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(30),
                    },
                ]),
            };
            service
                .update_failure_actions(actions)
                .context("setting service recovery/failure actions (sc.exe failure)")?;
        }

        tracing::info!(
            service = Self::SERVICE_NAME,
            mode = %cfg.mode,
            account = ?cfg.account,
            "installed Windows service with AutoStart + recovery actions"
        );
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let scm = Self::open_manager(ServiceManagerAccess::CONNECT)?;
        let service = scm
            .open_service(&self.service_name, ServiceAccess::DELETE)
            .context("opening the diyRAG service for deletion")?;
        service
            .delete()
            .context("deleting the diyRAG service (sc.exe delete)")?;
        tracing::info!(service = Self::SERVICE_NAME, "uninstalled Windows service");
        Ok(())
    }

    fn start(&self) -> Result<()> {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let scm = Self::open_manager(ServiceManagerAccess::CONNECT)?;
        let service = scm
            .open_service(&self.service_name, ServiceAccess::START)
            .context("opening the diyRAG service to start it")?;
        // No extra args — the registered `launch_arguments` carry `service --mode`.
        service
            .start::<&str>(&[])
            .context("starting the diyRAG service (sc.exe start)")?;
        tracing::info!(service = Self::SERVICE_NAME, "start requested");
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let scm = Self::open_manager(ServiceManagerAccess::CONNECT)?;
        let service = scm
            .open_service(&self.service_name, ServiceAccess::STOP)
            .context("opening the diyRAG service to stop it")?;
        // `diyragd`'s control handler turns this into a graceful drain (§16b.2).
        let _status = service
            .stop()
            .context("stopping the diyRAG service (sc.exe stop)")?;
        tracing::info!(
            service = Self::SERVICE_NAME,
            "stop requested (graceful drain)"
        );
        Ok(())
    }

    fn status(&self) -> Result<()> {
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::ServiceManagerAccess;

        let scm = Self::open_manager(ServiceManagerAccess::CONNECT)?;
        let service = scm
            .open_service(&self.service_name, ServiceAccess::QUERY_STATUS)
            .context("opening the diyRAG service to query status")?;
        let status = service
            .query_status()
            .context("querying diyRAG service status (sc.exe query)")?;
        // TODO: render as the standard structured envelope; for now print state.
        println!(
            "diyRAG (windows-scm): current_state={:?}",
            status.current_state
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Linux (generic): systemd (§16b.3)
// ---------------------------------------------------------------------------

/// Manage the diyRAG service via `systemctl` (generic Linux, §16b.3).
///
/// Wraps the documented commands:
/// ```text
/// systemctl enable --now diyragd     # install + start (boot autostart)
/// systemctl disable --now diyragd    # uninstall (stop + disable)
/// systemctl start|stop|restart diyragd
/// systemctl status diyragd
/// ```
/// The unit file itself (`deploy/systemd/diyragd.service`, `Restart=always`,
/// `WantedBy=multi-user.target`) ships in the repo; `install` enables it.
#[cfg(unix)]
pub struct Systemd {
    unit: String,
}

#[cfg(unix)]
impl Systemd {
    const UNIT: &'static str = "diyragd";

    pub fn new() -> Self {
        Self {
            unit: Self::UNIT.to_string(),
        }
    }

    /// Run `systemctl <args…>` and map a non-zero exit to an error.
    fn systemctl(&self, args: &[&str]) -> Result<()> {
        let status = std::process::Command::new("systemctl")
            .args(args)
            .status()
            .with_context(|| format!("spawning `systemctl {}`", args.join(" ")))?;
        if !status.success() {
            anyhow::bail!("`systemctl {}` failed: {status}", args.join(" "));
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Default for Systemd {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl ServiceManager for Systemd {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn install(&self, _cfg: &ServiceConfig) -> Result<()> {
        // `enable --now` => start now AND start on boot (the autostart, §16b.3).
        // TODO: optionally render+write deploy/systemd/diyragd.service to
        // /etc/systemd/system from `cfg` (binary_path, mode, account=User=) and
        // `daemon-reload` first, so install is self-contained on a fresh host.
        self.systemctl(&["enable", "--now", &self.unit])
    }

    fn uninstall(&self) -> Result<()> {
        self.systemctl(&["disable", "--now", &self.unit])
    }

    fn start(&self) -> Result<()> {
        self.systemctl(&["start", &self.unit])
    }

    fn stop(&self) -> Result<()> {
        self.systemctl(&["stop", &self.unit])
    }

    fn restart(&self) -> Result<()> {
        // systemd has an atomic restart; override the default stop-then-start.
        self.systemctl(&["restart", &self.unit])
    }

    fn status(&self) -> Result<()> {
        // `status` exits non-zero when the unit is inactive/failed, which is not
        // a CLI error — surface the output regardless of exit code.
        let _ = std::process::Command::new("systemctl")
            .args(["status", &self.unit])
            .status()
            .with_context(|| format!("spawning `systemctl status {}`", self.unit))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// unraid / any Docker host: Docker Compose (§16b.3)
// ---------------------------------------------------------------------------

/// Manage the diyRAG stack via `docker compose` — the unraid path (§16b.3).
///
/// Wraps:
/// ```text
/// docker compose -f <file> up -d     # install/start (with restart: unless-stopped)
/// docker compose -f <file> down      # uninstall/stop
/// docker compose -f <file> ps        # status
/// ```
/// Boot autostart on unraid is provided by `restart: unless-stopped` in the
/// compose file plus unraid auto-starting Docker on array start — no SCM/systemd
/// involvement (§16b.3).
pub struct DockerCompose {
    /// Path to the compose file; configurable for the unraid `appdata` layout
    /// (`/mnt/user/appdata/diyrag/docker-compose.yml`).
    compose_file: String,
}

impl DockerCompose {
    /// Default compose file path. DECISION: relative `docker-compose.yml` so the
    /// CLI works from the repo root; unraid users point `--compose-file` at
    /// `/mnt/user/appdata/diyrag/docker-compose.yml` (documented in README).
    const DEFAULT_COMPOSE_FILE: &'static str = "docker-compose.yml";

    pub fn new() -> Self {
        Self {
            compose_file: Self::DEFAULT_COMPOSE_FILE.to_string(),
        }
    }

    /// Override the compose file path (from `--compose-file`).
    pub fn with_compose_file(mut self, path: impl Into<String>) -> Self {
        self.compose_file = path.into();
        self
    }

    /// Run `docker compose -f <file> <args…>` (note: `compose` is a subcommand
    /// of `docker`, not the legacy `docker-compose` binary).
    fn compose(&self, args: &[&str]) -> Result<()> {
        let mut full = vec!["compose", "-f", &self.compose_file];
        full.extend_from_slice(args);
        let status = std::process::Command::new("docker")
            .args(&full)
            .status()
            .with_context(|| format!("spawning `docker {}`", full.join(" ")))?;
        if !status.success() {
            anyhow::bail!("`docker {}` failed: {status}", full.join(" "));
        }
        Ok(())
    }
}

impl Default for DockerCompose {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for DockerCompose {
    fn name(&self) -> &'static str {
        "docker-compose"
    }

    fn install(&self, _cfg: &ServiceConfig) -> Result<()> {
        // `up -d` brings the stack up detached; `restart: unless-stopped` in the
        // compose file gives boot-autostart on unraid array start (§16b.3).
        self.compose(&["up", "-d"])
    }

    fn uninstall(&self) -> Result<()> {
        // `down` stops + removes containers; named volumes (Postgres/Qdrant/blob)
        // are retained unless `-v` is passed — honors the retention spirit (§6.6).
        self.compose(&["down"])
    }

    fn start(&self) -> Result<()> {
        self.compose(&["up", "-d"])
    }

    fn stop(&self) -> Result<()> {
        // `stop` (not `down`) keeps the containers so `start` is fast.
        self.compose(&["stop"])
    }

    fn status(&self) -> Result<()> {
        self.compose(&["ps"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_compose_default_file() {
        let dc = DockerCompose::new();
        assert_eq!(dc.compose_file, "docker-compose.yml");
        let dc = dc.with_compose_file("/mnt/user/appdata/diyrag/docker-compose.yml");
        assert_eq!(
            dc.compose_file,
            "/mnt/user/appdata/diyrag/docker-compose.yml"
        );
        assert_eq!(dc.name(), "docker-compose");
    }

    #[test]
    fn default_manager_matches_platform() {
        let kind = default_manager_kind();
        if cfg!(windows) {
            assert_eq!(kind, ManagerKind::Scm);
        } else {
            assert_eq!(kind, ManagerKind::Systemd);
        }
    }

    #[test]
    fn select_docker_works_on_any_platform() {
        let over = ManagerOverride {
            manager: Some(ManagerKind::Docker),
            compose_file: "docker-compose.yml".to_string(),
        };
        let mgr = select(&over).expect("docker manager selectable");
        assert_eq!(mgr.name(), "docker-compose");
    }
}
