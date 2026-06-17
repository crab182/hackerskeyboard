# `diyragd` — diyRAG supervisor daemon & native service host

`diyragd` is the **single supervisor binary** for a diyRAG node
(MASTER_BUILD_SPEC.md §16b). It runs the Rust services per *run-mode* and
integrates with the host's service manager so a node **comes back automatically
after a reboot** — the explicit requirement (acceptance #9):

> *"the native windows app to run as a service upon device restart and via
> terminal commands to run on unraid."*

It is built with `#![forbid(unsafe_code)]`, `anyhow` at the binary boundary, and
`tracing` for structured logs (§19). It pairs with the `diyrag` CLI
(crate `diyrag-cli`), which *installs/controls* the service; `diyragd` is the
process that the service manager *invokes*.

## Run modes (`--mode`, spec §16b.1)

| Mode | Meaning |
|---|---|
| `all-in-one` (default) | Start every Rust service (`core-api`, `retrieval`, `ingestion-worker`, `mcp-server`, `sync-agent`, Rust-native inference backend) as supervised Tokio tasks in one process. The simplest shape for a single Windows box or homelab node. |
| `agent` | Orchestrate an external service set (the Docker Compose stack on unraid/Linux) and report health. |
| `service:<name>` | Run exactly one named service in this process (used by Compose replicas / sharded deploys). |

## Entry paths

All three converge on the cross-platform, cancellable `Supervisor`
(`src/supervisor.rs`). Cancellation drives a **graceful drain**: in-flight work
units finish, NATS messages are ack/nak'd, logs flush, then the process exits.

```
                         ┌──────────────────────────────────────────┐
  Windows SCM ──"service"─▶ windows_service::run → SCM dispatcher ──┐ │
  systemd/Docker ─"service"▶ unix::run (SIGTERM/SIGINT)             ├─▶ Supervisor::run(cancel)
  console ──"run"/bare ────▶ unix::run | windows_service::run_      ┘ │   (graceful drain on cancel)
                              interactive (Ctrl-C)                     │
                         └──────────────────────────────────────────┘
```

- `src/main.rs` — clap CLI. `diyragd service --mode <…>` is what the service
  manager invokes; `diyragd run`/bare `diyragd` is the foreground/dev path.
- `src/windows_service.rs` (`#[cfg(windows)]`) — SCM bridge: `define_windows_
  service!`, `service_dispatcher::start`, a control handler for
  `Stop`/`Shutdown`/`Interrogate`, status transitions
  `StartPending → Running → StopPending → Stopped`, Event Log start/stop lines.
- `src/unix.rs` (`#[cfg(unix)]`) — Tokio runtime + SIGTERM/SIGINT → cancel, for
  systemd `Restart=always` and the Docker entrypoint.

## Boot-autostart story

### Windows (Service Control Manager)

`diyrag service install` (see `diyrag-cli`) registers the service with
`StartType::AutoStart` and `ServiceType::OWN_PROCESS`, under a dedicated
low-privilege account, with recovery actions. From then on Windows launches:

```
C:\Program Files\diyRAG\diyragd.exe service --mode all-in-one
```

on **every device restart**, before any interactive logon — that is the
autostart. The `service` subcommand connects to the SCM
(`service_dispatcher::start`); on `Stop`/`Shutdown` the control handler fires the
supervisor's `CancellationToken` and reports `StopPending` then `Stopped`.

Raw SCM equivalent (reference / fallback):

```powershell
sc.exe create diyRAG binPath= "C:\Program Files\diyRAG\diyragd.exe service --mode all-in-one" start= auto
sc.exe failure diyRAG reset= 86400 actions= restart/5000/restart/10000/restart/30000
sc.exe start diyRAG
```

State lives under `%ProgramData%\diyRAG\` with restricted ACLs; logs go to the
**Windows Event Log** (via `eventlog`) *and* rolling JSON files (§16b.2).

### Linux — systemd

`deploy/systemd/diyragd.service` (a unit with `Restart=always`,
`WantedBy=multi-user.target`) runs `diyragd service --mode all-in-one`.
`diyrag service install` wraps `systemctl enable --now`, so the daemon starts on
boot and is restarted on crash. SIGTERM (from `systemctl stop`) drains it.

### unraid — Docker

`diyragd` is the container entrypoint. Docker's `restart: unless-stopped` plus
unraid auto-starting Docker on array start brings the stack back after a reboot
(the Linux analog of the Windows-Service autostart). `docker stop` sends SIGTERM
to PID 1, which `unix::run` translates into a graceful drain.

## Build & run

```bash
# foreground, all-in-one (any platform)
cargo run -p diyrag-diyragd

# explicit mode
cargo run -p diyrag-diyragd -- run --mode agent

# the service-manager entry point (what the SCM / systemd / Docker invoke)
diyragd service --mode all-in-one
```

Logs honor `RUST_LOG` (default `info`) and emit structured JSON to stdout
(journald / `docker logs`) plus, on Windows, the Event Log.

## Status

The supervisor wiring (run-mode dispatch, supervised restart-with-backoff,
cancellation/drain, both platform entry paths) is complete and unit-tested. The
per-service bodies in `run_service_entrypoint` are `// TODO:` stubs that park
until cancelled; they will be replaced by the sibling crates' `serve(cancel)`
library entry points as those land (see §20 M9).
