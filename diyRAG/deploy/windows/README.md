# diyRAG on Windows — Service quickstart

> MASTER_BUILD_SPEC.md §16b.2 (native Windows Service), §16 + `GPU.md`,
> §12.8 (hardening), §14 (recovery), acceptance #9.

diyRAG runs on Windows as a **Windows Service** (`diyragd`, internal name
`diyRAG`) that **auto-starts on every device restart** and is controlled from a
terminal with `diyrag service start|stop|status`. The Tauri GUI talks to this
same local service, so closing the GUI never stops ingestion, and the service
keeps running across reboots.

Files in this folder:

| File | Purpose |
|---|---|
| `install.ps1` | Installs binaries, ACL-locks data, **registers the auto-start service** (CLI-preferred, `sc.exe` fallback). |
| `uninstall.ps1` | Stops + deletes the service, removes binaries; **keeps data by default** (`-RemoveData` to wipe). |
| `GPU.md` | CUDA / DirectML execution-provider notes; GPU under a session-0 service. |
| `winsw-fallback.md` | WinSW/NSSM wrapper approach for shops avoiding native SCM integration. |

---

## Prerequisites

- Windows 10 21H2+ or Windows 11, **elevated (Administrator)** PowerShell
  (5.1+ or 7+).
- The built binaries `diyragd.exe` and `diyrag.exe` (from `cargo build
  --release`, or the MSI/winget package). Both should be Authenticode-signed
  before distribution (§16b.2).
- For GPU: a machine-wide GPU driver (and CUDA runtime if using the CUDA EP) —
  see [`GPU.md`](./GPU.md).
- This is the `all-in-one` single-box runtime. Postgres/Qdrant/MinIO are
  reachable per your `%ProgramData%\diyRAG\config\diyrag.env` (a single-binary
  appliance bundling them is a backlog item, §23 #11).

---

## End-to-end quickstart

### 1. Install (registers the auto-start service)

From an **elevated** PowerShell, in this folder (or wherever the binaries are):

```powershell
# Preferred: install.ps1 wraps `diyrag service install` and falls back to sc.exe.
.\install.ps1
```

What `install.ps1` does (see its header for detail): copies the binaries to
`C:\Program Files\diyRAG\`, creates `%ProgramData%\diyRAG\{config,certs,models,logs}`
with **restrictive ACLs** (Administrators + SYSTEM + the service account only),
seeds a placeholder `config\diyrag.env` (**no secrets**), and registers the
service with **StartType = Automatic** plus crash-recovery actions
(restart at 5s / 10s / 30s, §14). It then starts the service.

Equivalent explicit invocations:

```powershell
# Pick a different low-privilege account, or agent mode:
.\install.ps1 -ServiceAccount "NT SERVICE\diyRAG" -Mode all-in-one

# Skip the CLI wrapper and register via raw sc.exe:
.\install.ps1 -UseScFallback

# Or call the CLI directly (what install.ps1 does under the hood):
diyrag service install --mode all-in-one --auto-start --account "NT SERVICE\diyRAG"
diyrag service start
```

> Avoiding native SCM integration entirely? See
> [`winsw-fallback.md`](./winsw-fallback.md) for the WinSW/NSSM wrapper.

### 2. The service auto-starts every reboot

`StartType = Automatic` means the SCM starts `diyRAG` on every device restart —
including before any user signs in (it runs headless in session 0, GPU
included; see `GPU.md`). No further action needed after install.

### 3. Manage it from a terminal

The control surface is identical to Linux/unraid (§16b.4):

```powershell
diyrag service status     # Running/Stopped + uptime + restart count (§13.3)
diyrag service stop       # graceful: drains in-flight work, acks/naks NATS, flushes logs
diyrag service start
diyrag service restart
```

Drive the platform headlessly with the same CLI (auth via API key/OAuth):

```powershell
diyrag ingest "D:\Documents" --watch
diyrag query "What changed in the Q3 policy?" --answer
diyrag node status
```

Raw SCM equivalents (reference / no-CLI environments):

```powershell
sc.exe query  diyRAG
sc.exe stop   diyRAG
sc.exe start  diyRAG
Get-Service diyRAG
```

### 4. The Tauri GUI talks to the local service

Launch the diyRAG desktop app. It **detects the local `diyRAG` service**, shows
its status on the Dashboard, and exposes start/stop/restart (elevating via UAC)
in Admin → service control (§10.2, §16b.2). Closing the GUI does **not** stop
the service — ingestion and queries continue, and the service comes back on the
next reboot.

---

## Verify-after-reboot checklist (acceptance #9)

Run this after a real or simulated restart to prove the boot-autostart and
graceful-recovery guarantees:

- [ ] **Reboot the machine** (or `Restart-Computer`).
- [ ] **Before signing in is not required** — but after boot, open an elevated
      PowerShell.
- [ ] **Service is Running automatically:**
      ```powershell
      Get-Service diyRAG                                  # Status = Running
      (Get-CimInstance Win32_Service -Filter "Name='diyRAG'").StartMode  # Auto
      ```
- [ ] **CLI agrees:** `diyrag service status` reports Running, with an uptime
      consistent with the reboot time and the expected restart count.
- [ ] **Health endpoints green:** the gateway answers locally —
      ```powershell
      Invoke-WebRequest https://localhost:8443/healthz -SkipCertificateCheck
      Invoke-WebRequest https://localhost:8443/readyz  -SkipCertificateCheck
      ```
      (8443 is the api-gateway port, matching the Docker stack; §11, docker-compose.yml.)
- [ ] **Recovery config intact:** `sc.exe qc diyRAG` shows quoted binPath and
      `start= auto`; `sc.exe qfailure diyRAG` shows the restart actions (§14).
- [ ] **Event Log clean:** Application log shows `diyRAG` starting, the GPU EP
      it selected (not a silent CPU fallback; `GPU.md`), and no ACL/permission
      errors.
- [ ] **Functional smoke:** `diyrag query "smoke test" --answer` returns a cited
      answer; a watched-folder drop gets ingested.
- [ ] **Graceful stop drains:** `diyrag service stop` returns only after
      in-flight work units drain; `diyrag service start` resumes with no
      duplicate chunks (idempotency, §6.2 / §21).

---

## Uninstall

```powershell
.\uninstall.ps1                  # stop + delete service, KEEP all data
.\uninstall.ps1 -RemoveData -Force   # also wipe %ProgramData%\diyRAG (irreversible)
```

By default the data tree under `%ProgramData%\diyRAG` is preserved so an
accidental uninstall never destroys ingested content (mirrors §1 / §6.6).

---

## Troubleshooting

- **Service won't start after reboot:** check the Application Event Log; common
  causes are a missing dependency (Docker Desktop only in `agent` mode), a
  config error in `diyrag.env`, or the service account lacking ACLs on
  `%ProgramData%\diyRAG` (re-run `install.ps1` once the virtual account has
  resolved — its header notes this).
- **GPU silently on CPU:** see `GPU.md` — usually a CUDA/cuDNN version mismatch
  or a per-user (not machine-wide) driver install.
- **`diyrag` not found:** it lives in `C:\Program Files\diyRAG\`; add to `PATH`
  or call by full path. The MSI/winget package puts it on `PATH` automatically.
