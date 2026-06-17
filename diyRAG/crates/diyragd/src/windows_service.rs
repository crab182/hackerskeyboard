//! Windows Service integration (MASTER_BUILD_SPEC.md §16b.2).
//!
//! This module is `#[cfg(windows)]`-gated and is the bridge between the Windows
//! **Service Control Manager (SCM)** and the cross-platform [`Supervisor`]
//! ([`crate::supervisor`]).
//!
//! ## Boot autostart story (the explicit requirement, §16b.2)
//! The service is *registered* with `StartType::AutoStart` by
//! `diyrag service install` (see `diyrag-cli`'s `ServiceManager::WindowsScm`),
//! **not** here — registration is a one-time privileged operation, whereas this
//! module is the code the SCM *invokes* on every boot. Once registered with
//! AutoStart, Windows launches `diyragd.exe service --mode <…>` automatically on
//! every device restart, before any interactive logon. That is what makes the
//! RAG node "come back after a reboot" (acceptance #9).
//!
//! ## Lifecycle (§16b.2)
//! 1. `main` detects SCM launch (the `service` subcommand) and calls [`run`].
//! 2. [`run`] hands `ffi_service_main` to `service_dispatcher::start`, which
//!    blocks on the SCM connection (`StartServiceCtrlDispatcher`).
//! 3. The SCM calls `service_main`, which:
//!    * registers a control handler for `Stop | Shutdown | Interrogate`,
//!    * reports `Running`,
//!    * builds a Tokio runtime and runs the [`Supervisor`] with a
//!      [`CancellationToken`],
//!    * on `Stop`/`Shutdown`: reports `StopPending`, fires the token (graceful
//!      drain — cancel work, ack/nak NATS, flush logs), then reports `Stopped`.
//! 4. A line is written to the Windows Event Log on start and on stop.
//!
//! ## windows-service 0.7 API surface assumed (see report notes)
//! * `define_windows_service!(ffi_service_main, service_main)` generates the
//!   `extern "system"` FFI shim and calls `service_main(arguments: Vec<OsString>)`.
//! * `service_dispatcher::start(name, ffi_service_main)` connects to the SCM.
//! * `service_control_handler::register(name, event_handler)` returns a
//!   `ServiceStatusHandle`; the handler returns `ServiceControlHandlerResult`.
//! * `ServiceControl::{Stop, Shutdown, Interrogate}` are the variants we accept.
//! * `ServiceStatus { service_type, current_state, controls_accepted,
//!   exit_code, checkpoint, wait_hint, process_id }` is set via
//!   `status_handle.set_service_status(...)`.
//! * `ServiceState::{StartPending, Running, StopPending, Stopped}`,
//!   `ServiceControlAccept::{STOP, SHUTDOWN}`,
//!   `ServiceExitCode::Win32(u32)`, `ServiceType::OWN_PROCESS`.

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
    ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

use crate::supervisor::{RunMode, Supervisor};

/// The SCM service name. MUST match `diyrag service install` / the `sc.exe`
/// reference in spec §16b.2 (`sc.exe create diyRAG …`).
pub const SERVICE_NAME: &str = "diyRAG";

/// Event Log source name registered by the installer.
const EVENT_LOG_SOURCE: &str = "diyRAG";

/// `OWN_PROCESS`: the service runs in its own `diyragd.exe` process (§16b.2).
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

// Generate `ffi_service_main`, the `extern "system"` entry point the SCM calls,
// which in turn invokes our safe `service_main(args: Vec<OsString>)`.
windows_service::define_windows_service!(ffi_service_main, service_main);

/// Stash the run-mode for `service_main`, which the SCM invokes with only the
/// service-start arguments (not our process argv). `diyrag service install`
/// bakes `--mode <…>` into the registered `binPath=`, so the dispatcher path
/// also passes it through argv; we keep a process-global fallback for the case
/// where the SCM start arguments are empty.
static SELECTED_MODE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Entry point from `main` when launched by the SCM (the `service` subcommand).
///
/// Blocks on `service_dispatcher::start`, which only returns once the service
/// has stopped. `mode_raw` is the `--mode` value parsed from argv.
pub fn run(mode_raw: String) -> Result<()> {
    init_tracing();
    // Best-effort: register the Event Log source so start/stop lines have a home.
    // Registration of the source key itself is done by the installer (it needs
    // admin); here we only initialise the logger handle.
    let _ = init_event_log();
    let _ = SELECTED_MODE.set(mode_raw);

    // Hands control to the SCM; calls `ffi_service_main` → `service_main`.
    // NOTE (windows-service 0.7): if the process was NOT launched by the SCM,
    // `service_dispatcher::start` fails with
    // ERROR_FAILED_SERVICE_CONTROLLER_CONNECT (1063). The interactive console
    // path uses [`run_interactive`] instead, so this is only reached under SCM.
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| anyhow::anyhow!("failed to start SCM service dispatcher: {e}"))?;
    Ok(())
}

/// Interactive (non-SCM) run for a developer console on Windows.
///
/// Mirrors the Unix foreground runner: build a Tokio runtime, start the
/// [`Supervisor`], and cancel on Ctrl-C (`tokio::signal::ctrl_c`, which maps to
/// the Windows console `CTRL_C_EVENT`). This is the path taken by `diyragd run`
/// / bare `diyragd` on Windows — it does NOT talk to the SCM (spec §16b.2: the
/// SCM path is the `service` subcommand only).
pub fn run_interactive(mode_raw: String) -> Result<()> {
    init_tracing();
    let mode = RunMode::parse(&mode_raw)?;
    tracing::info!(?mode, "diyragd starting (windows: interactive console)");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build Tokio runtime: {e}"))?;

    runtime.block_on(async move {
        let supervisor = Supervisor::new(mode);
        let cancel = CancellationToken::new();
        let supervisor_cancel = cancel.clone();
        let task = tokio::spawn(async move { supervisor.run(supervisor_cancel).await });

        // Ctrl-C → graceful drain.
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "failed to listen for Ctrl-C; cancelling anyway");
        }
        tracing::info!("Ctrl-C received; cancelling supervisor (graceful drain)");
        cancel.cancel();

        match task.await {
            Ok(result) => result,
            Err(join_err) => Err(anyhow::anyhow!("supervisor task panicked: {join_err}")),
        }
    })?;

    tracing::info!("diyragd stopped (drained)");
    Ok(())
}

/// Best-effort tracing init for the Windows paths (rolling stdout JSON, §13.1).
///
/// DECISION: kept self-contained (not `diyrag_common::logging`) to match the
/// Unix path until the typed config crate is wired here. The Windows **Event
/// Log** sink is initialised separately in [`init_event_log`]; this provides the
/// structured-JSON file/console stream the spec also requires (§16b.2).
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().json().with_env_filter(filter).try_init();
}

/// Initialise the Windows Event Log `tracing`/logging sink (§16b.2). The source
/// must already be registered (installer step); failure is non-fatal — we still
/// log to rolling JSON files.
fn init_event_log() -> Result<()> {
    // `eventlog` 0.3 exposes `eventlog::init(source, level)` / a `register`
    // helper used at install time. We assume `init` wires a global logger.
    eventlog::init(EVENT_LOG_SOURCE, log::Level::Info)
        .map_err(|e| anyhow::anyhow!("eventlog init failed: {e}"))?;
    Ok(())
}

/// Write a single Event Log line (start/stop milestones, §16b.2).
fn event_log_line(msg: &str) {
    // Routed through the `log` facade that `eventlog::init` backs.
    log::info!("{msg}");
    tracing::info!(target: "windows_eventlog", "{msg}");
}

/// The SCM-invoked service body. Runs on a dedicated thread spawned by the
/// dispatcher; we build our own Tokio runtime here rather than using
/// `#[tokio::main]` because the SCM owns this thread.
fn service_main(arguments: Vec<OsString>) {
    if let Err(err) = run_service(arguments) {
        // Surface to the Event Log; the process will exit non-zero and the SCM
        // recovery actions (set at install time) restart us (§14 / §16b.2).
        event_log_line(&format!("diyRAG service exited with error: {err}"));
        tracing::error!(error = %err, "windows service_main failed");
    }
}

fn run_service(arguments: Vec<OsString>) -> Result<()> {
    // Resolve the run-mode: prefer SCM start arguments, fall back to the value
    // captured in `run` from our own argv (baked into `binPath=` at install).
    let mode = resolve_mode(&arguments)?;
    event_log_line(&format!("diyRAG service starting (mode = {mode:?})"));

    // Channel the control handler uses to ask the worker thread to stop.
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    // Register the control handler. It must be fast and non-blocking: it only
    // signals the worker; the worker performs the graceful drain.
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            // Health probe — report current status (handled by returning NoError;
            // the SCM re-reads the last `set_service_status`).
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            // Both Stop and Shutdown trigger a graceful drain.
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
        .map_err(|e| anyhow::anyhow!("failed to register control handler: {e}"))?;

    // Tell the SCM we are starting, then running.
    set_status(&status_handle, ServiceState::StartPending, Duration::from_secs(10), ServiceControlAccept::empty())?;

    // The supervisor's cancellation token, fired from the control handler path.
    let cancel = CancellationToken::new();

    // Build a multi-thread Tokio runtime to host the Supervisor. We run it on a
    // background thread so this thread can pump the SCM shutdown channel.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build Tokio runtime: {e}"))?;

    let supervisor = Supervisor::new(mode);
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || -> Result<()> {
        runtime.block_on(async move { supervisor.run(worker_cancel).await })
    });

    // We accept STOP and SHUTDOWN while Running (§16b.2 lifecycle).
    set_status(
        &status_handle,
        ServiceState::Running,
        Duration::default(),
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    )?;
    event_log_line("diyRAG service running");

    // Block until the SCM asks us to stop (Stop/Shutdown -> channel send).
    let _ = shutdown_rx.recv();

    // Graceful drain: announce StopPending, cancel the supervisor, await drain.
    event_log_line("diyRAG service stopping (draining in-flight work)");
    set_status(&status_handle, ServiceState::StopPending, Duration::from_secs(30), ServiceControlAccept::empty())?;

    cancel.cancel();
    // Join the supervisor; it returns only once every child has drained.
    let drain_result = worker
        .join()
        .map_err(|_| anyhow::anyhow!("supervisor worker thread panicked"))?;

    // Report Stopped regardless, but reflect any drain error in the exit code.
    let exit_code = match &drain_result {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(_) => ServiceExitCode::Win32(1),
    };
    set_status_with_exit(&status_handle, ServiceState::Stopped, exit_code)?;
    event_log_line("diyRAG service stopped");

    drain_result
}

/// Resolve the run-mode from SCM start arguments, falling back to the argv value
/// captured in [`run`]. Defaults to `all-in-one` (the documented Windows shape,
/// spec §24.5) if nothing was supplied.
fn resolve_mode(arguments: &[OsString]) -> Result<RunMode> {
    // SCM start args look like `["--mode", "all-in-one"]` when configured.
    let mut iter = arguments.iter();
    while let Some(arg) = iter.next() {
        if arg == "--mode" {
            if let Some(val) = iter.next() {
                return RunMode::parse(&val.to_string_lossy());
            }
        }
    }
    if let Some(raw) = SELECTED_MODE.get() {
        return RunMode::parse(raw);
    }
    Ok(RunMode::AllInOne)
}

/// Helper: set a status with no exit code (the common case).
fn set_status(
    handle: &service_control_handler::ServiceStatusHandle,
    state: ServiceState,
    wait_hint: Duration,
    controls_accepted: ServiceControlAccept,
) -> Result<()> {
    handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint,
            process_id: None,
        })
        .map_err(|e| anyhow::anyhow!("set_service_status({state:?}) failed: {e}"))
}

/// Helper: terminal status with an explicit exit code.
fn set_status_with_exit(
    handle: &service_control_handler::ServiceStatusHandle,
    state: ServiceState,
    exit_code: ServiceExitCode,
) -> Result<()> {
    handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .map_err(|e| anyhow::anyhow!("set_service_status({state:?}) failed: {e}"))
}
