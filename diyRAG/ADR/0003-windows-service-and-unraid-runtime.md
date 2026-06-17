# ADR-0003: Windows Service + unraid dual runtime
- Status: Accepted
- Date: 2026-06-17

## Context
Explicit requirement: the native app must "run as a service upon device restart and via terminal commands to run on unraid." This spans two very different hosts — a Windows desktop/server (SCM-managed services) and unraid (a Slackware-based NAS whose first-class app model is Docker). Both must auto-start after a reboot and be controllable from a terminal.

## Decision
Ship one supervisor binary **`diyragd`** and one control CLI **`diyrag`**.
- **Windows:** `diyragd` integrates with the Service Control Manager via the `windows-service` crate (`define_windows_service!`, control handler for Stop/Shutdown/Interrogate, status reporting, graceful Tokio cancellation). `diyrag service install` registers it with `StartType::AutoStart` (starts on every boot), `ServiceType::OWN_PROCESS`, a dedicated low-privilege account, and SCM failure/recovery actions. Default run mode is `all-in-one` (services as Tokio tasks in one process).
- **unraid:** Docker Compose stack + a Community Apps template (`deploy/unraid/diyrag.xml`) with `restart: unless-stopped` so the stack returns on array start; a User Scripts "At Startup of Array" hook is the explicit boot trigger. `diyragd --mode agent` can orchestrate Compose.
- **Generic Linux:** `deploy/systemd/diyragd.service` (`Restart=always`, hardened).
- A single **`ServiceManager`** trait with `WindowsScm` / `Systemd` / `DockerCompose` impls gives the CLI an identical surface everywhere; the impl is chosen by `cfg!` with a `--manager` override.

## Consequences
**Easier:** one mental model and one CLI across Windows, unraid, and Linux; boot-persistence is native on each platform; the Tauri GUI manages the same local service.

**Harder:** three OS-integration code paths to test (CI matrix); Windows GPU under session 0 needs the Rust-native backend (ADR-0004) since vLLM is Linux-only; MSI/winget packaging + Authenticode signing add release steps.

**Follow-ups:** verify-after-reboot tests on both platforms; document the low-priv service account and ACL-locked install dir (§12.8); WinSW/NSSM fallback for shops avoiding native SCM.

## Alternatives considered
- **Docker on Windows only** (WSL2/Docker Desktop) — avoids SCM code but isn't a true OS service, has GPU friction, and is a heavier dependency for a desktop user. Offered as `--mode agent`, not the default.
- **NSSM/WinSW wrapper as the primary** — simple but external and less controllable than native SCM integration; kept as a documented fallback.
