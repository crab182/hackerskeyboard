# WinSW / NSSM fallback wrapper (Windows)

> MASTER_BUILD_SPEC.md §16b.2 ("**Fallback** wrapper (**WinSW** or **NSSM**)
> documented for environments that prefer not to use the native SCM
> integration"), §3.2, §14, §22 #13.

## When to use this (and when NOT to)

The **preferred** path is the native Service Control Manager (SCM) integration
baked into `diyragd` via the `windows-service` crate, registered by:

```powershell
diyrag service install --mode all-in-one --auto-start --account "NT SERVICE\diyRAG"
```

…or by `deploy/windows/install.ps1` (which wraps that CLI and falls back to raw
`sc.exe`). Use the native path whenever you can — it gives `diyragd` a real
`service_main`, a control handler for `Stop`/`Shutdown`, graceful Tokio
shutdown (drain in-flight work units, ack/nak NATS, flush logs), and Windows
Event Log integration (§16b.2).

Reach for a **WinSW/NSSM wrapper only** when a shop:

- standardizes all services behind one wrapper toolchain for inventory/audit,
- wants a service for a build of `diyragd` that does **not** yet implement the
  SCM `service` subcommand (e.g. an early/dev binary run as a console app), or
- has policy against binaries calling `StartServiceCtrlDispatcher` directly.

Trade-off: a wrapper supervises `diyragd` as a **console child process**. On
`Stop` the wrapper sends a CTRL+C / kill rather than a native SCM `Stop`
control, so `diyragd` must treat console signals as a graceful-shutdown
trigger to still drain work. Prefer WinSW (it forwards a stop signal and
supports a kill timeout) over NSSM for graceful drain.

Either way, the binary should be **Authenticode-signed** and live in an
ACL-locked directory (§12.8, §22 #13). Do **not** run the wrapper as
LocalSystem unless GPU access forces it; use a dedicated low-privilege account
just like the native install.

---

## Option A — WinSW (recommended wrapper)

[WinSW](https://github.com/winsw/winsw) is a single `.exe` that reads an XML
config sibling to it and runs your program as a service. Convention: name the
wrapper `diyRAG-service.exe` and place `diyRAG-service.xml` next to it.

### XML config sketch — `diyRAG-service.xml`

```xml
<service>
  <!-- SCM internal name == the native service name so the diyrag CLI and the
       after-reboot checklist still find it (sc.exe query diyRAG, etc.). -->
  <id>diyRAG</id>
  <name>diyRAG Retrieval-Augmented Generation Service</name>
  <description>
    diyRAG supervisor (diyragd) running in all-in-one mode, wrapped by WinSW.
    Auto-starts on every device restart; control via `diyrag service ...`.
  </description>

  <!-- The supervisor binary and its run mode (§16b.1). -->
  <executable>C:\Program Files\diyRAG\diyragd.exe</executable>
  <arguments>service --mode all-in-one</arguments>
  <workingdirectory>C:\Program Files\diyRAG</workingdirectory>

  <!-- AUTO-START ON BOOT (the headline requirement). -->
  <startmode>Automatic</startmode>
  <delayedAutoStart>false</delayedAutoStart>

  <!-- Run under the dedicated low-privilege account, NOT LocalSystem
       (§12.8, §22 #13). For a managed virtual account, omit <serviceaccount>
       and instead set the logon account with sc.exe after install:
         sc.exe config diyRAG obj= "NT SERVICE\diyRAG"
       (gMSA/virtual accounts have no password and are preferred.) -->
  <serviceaccount>
    <username>.\diyRAGsvc</username>
    <password>__SET_VIA_SECRET_STORE__</password>   <!-- PLACEHOLDER, never commit -->
    <allowservicelogon>true</allowservicelogon>
  </serviceaccount>

  <!-- Config + secrets come from the env file written by install.ps1; no
       secrets in this XML (§12.1, §19). Point diyragd at %ProgramData%. -->
  <env name="DIYRAG_DATA_DIR"   value="C:\ProgramData\diyRAG"/>
  <env name="DIYRAG_CONFIG_DIR" value="C:\ProgramData\diyRAG\config"/>
  <env name="DIYRAG_CERTS_DIR"  value="C:\ProgramData\diyRAG\certs"/>
  <env name="RUST_LOG"          value="info,diyrag=info"/>
  <env name="DIYRAG_LOG_FORMAT" value="json"/>

  <!-- CRASH RECOVERY (§14): restart on unexpected exit with backoff. WinSW
       resets the restart counter after the service has run this long. -->
  <onfailure action="restart" delay="5 sec"/>
  <onfailure action="restart" delay="10 sec"/>
  <onfailure action="restart" delay="30 sec"/>
  <resetfailure>1 hour</resetfailure>

  <!-- Graceful stop: give diyragd time to drain in-flight work units before
       WinSW force-kills it (§16b.2). diyragd should treat the console stop
       signal as a graceful-shutdown request. -->
  <stoptimeout>30 sec</stoptimeout>
  <stopparentprocessfirst>true</stopparentprocessfirst>

  <!-- Rolling JSON logs alongside the Windows Event Log (§13.1, §16b.2). -->
  <logpath>C:\ProgramData\diyRAG\logs</logpath>
  <log mode="roll-by-size">
    <sizeThreshold>10240</sizeThreshold>  <!-- KB -->
    <keepFiles>14</keepFiles>
  </log>
</service>
```

### Install / control (terminal)

```powershell
# From an elevated PowerShell, in the folder holding diyRAG-service.exe + .xml:
.\diyRAG-service.exe install      # registers the service (Automatic start)
.\diyRAG-service.exe start
.\diyRAG-service.exe status
.\diyRAG-service.exe stop
.\diyRAG-service.exe uninstall

# Lock the logon account to the managed virtual account afterwards:
sc.exe config diyRAG obj= "NT SERVICE\diyRAG"
```

Because the WinSW service `id` is `diyRAG`, the standard verification commands
from `README.md` still apply (`sc.exe qc diyRAG`, `Get-Service diyRAG`), and the
service still auto-starts after a reboot.

---

## Option B — NSSM (the "Non-Sucking Service Manager")

[NSSM](https://nssm.cc/) configures a service interactively or from the CLI.
No XML; settings live in the registry. Useful when you want a quick wrapper and
do not need WinSW's signal-forwarding niceties.

```powershell
# Elevated PowerShell. nssm.exe on PATH or referenced by full path.
nssm install diyRAG "C:\Program Files\diyRAG\diyragd.exe" "service --mode all-in-one"
nssm set diyRAG DisplayName "diyRAG Retrieval-Augmented Generation Service"
nssm set diyRAG Start SERVICE_AUTO_START                 # auto-start on boot
nssm set diyRAG AppDirectory "C:\Program Files\diyRAG"
nssm set diyRAG ObjectName "NT SERVICE\diyRAG"            # low-priv account (§12.8)

# Environment (no secrets here; real secrets via a secret store — §12.1):
nssm set diyRAG AppEnvironmentExtra ^
  DIYRAG_DATA_DIR=C:\ProgramData\diyRAG ^
  DIYRAG_CONFIG_DIR=C:\ProgramData\diyRAG\config ^
  DIYRAG_CERTS_DIR=C:\ProgramData\diyRAG\certs ^
  RUST_LOG=info,diyrag=info ^
  DIYRAG_LOG_FORMAT=json

# Crash recovery + graceful stop (§14, §16b.2):
nssm set diyRAG AppExit Default Restart
nssm set diyRAG AppRestartDelay 5000        # ms before restart
nssm set diyRAG AppStopMethodConsole 30000  # send CTRL+C, wait 30s to drain

# Rolling stdout/stderr to files (Event Log still comes from diyragd itself):
nssm set diyRAG AppStdout C:\ProgramData\diyRAG\logs\diyragd.out.log
nssm set diyRAG AppStderr C:\ProgramData\diyRAG\logs\diyragd.err.log
nssm set diyRAG AppRotateFiles 1
nssm set diyRAG AppRotateBytes 10485760

nssm start diyRAG
nssm status diyRAG
```

Remove with `nssm stop diyRAG` then `nssm remove diyRAG confirm`.

---

## Security notes (apply to BOTH wrappers)

- **Quote the binary path.** `C:\Program Files\...` contains a space; an
  unquoted service path is a classic local-privesc vector (§22 #13). Both WinSW
  (`<executable>`) and NSSM handle this, but verify with `sc.exe qc diyRAG`.
- **ACL-lock the install dir and `%ProgramData%\diyRAG`.** Run `install.ps1`'s
  ACL step (or its `Set-LockedAcl` logic) even when using a wrapper — a writable
  binary or config path defeats the low-privilege account (§12.8).
- **No secrets in the XML / registry.** Use placeholders; deliver real secrets
  via DPAPI or a secret store at runtime (§12.1, §19).
- **Sign the binaries.** `diyragd.exe`, `diyrag.exe`, and the wrapper `.exe`
  should be Authenticode-signed before distribution (§16b.2).
- **GPU under session 0.** A wrapped service still runs headless in session 0;
  the Rust-native `ort`/`mistral.rs` backend works there — see `GPU.md`.
