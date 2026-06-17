<#
.SYNOPSIS
    diyRAG Windows Service installer (MASTER_BUILD_SPEC.md §16b.2, §12.8).

.DESCRIPTION
    Installs the diyRAG supervisor (`diyragd.exe`) and control CLI (`diyrag.exe`)
    on a Windows host and registers `diyragd` as a Windows Service that
    AUTO-STARTS on every device restart (boot), controllable via
    `diyrag service start|stop|status`.

    What it does, in order:
      1. Verifies it is running elevated (Administrator). If not, prints the
         self-elevation command and exits non-zero. (No silent re-launch so the
         operator sees exactly what will run.)
      2. Copies diyragd.exe / diyrag.exe to  C:\Program Files\diyRAG\
      3. Creates %ProgramData%\diyRAG\{config,certs,models,logs} with RESTRICTIVE
         ACLs (Administrators + SYSTEM + the service account only; inheritance
         disabled). Certs/keys are never world-readable (§12.1, §12.8).
      4. Registers the service via the PREFERRED CLI path:
             diyrag service install --mode all-in-one --auto-start --account "<acct>"
         A raw `sc.exe create ... start= auto` + `sc.exe failure` equivalent is
         shown as a COMMENTED fallback below (and in -UseScFallback mode).
      5. Sets auto-start on boot and configures crash-recovery actions (§14).

    Idempotent: re-running updates the binaries, re-applies ACLs, and reconciles
    the service config without erroring if the service already exists.

.PARAMETER SourceDir
    Directory containing diyragd.exe and diyrag.exe (default: script directory).

.PARAMETER InstallDir
    Target install dir (default: C:\Program Files\diyRAG).

.PARAMETER ServiceAccount
    Service logon account (default: "NT SERVICE\diyRAG" managed virtual account).
    Do NOT use LocalSystem unless GPU access forces it (documented trade-off,
    §16b.2 / §22 #13).

.PARAMETER Mode
    diyragd run mode: all-in-one | agent (default: all-in-one).

.PARAMETER UseScFallback
    Skip the `diyrag service install` CLI path and register the service with the
    raw sc.exe commands instead (for environments without the CLI wrapper).

.EXAMPLE
    # Run from an elevated PowerShell:
    .\install.ps1

.EXAMPLE
    .\install.ps1 -ServiceAccount "NT AUTHORITY\LocalService" -Mode all-in-one

.NOTES
    Binaries should be Authenticode-signed before distribution (§16b.2, §22 #13).
    Requires admin. PowerShell 5.1+ or PowerShell 7+.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$SourceDir      = $PSScriptRoot,
    [string]$InstallDir     = (Join-Path $env:ProgramFiles 'diyRAG'),
    [string]$ServiceAccount = 'NT SERVICE\diyRAG',
    [ValidateSet('all-in-one', 'agent')]
    [string]$Mode           = 'all-in-one',
    [switch]$UseScFallback
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# --- Constants ---------------------------------------------------------------
$ServiceName  = 'diyRAG'                                   # WINDOWS_SERVICE_NAME
$DisplayName  = 'diyRAG Retrieval-Augmented Generation Service'
$DataDir      = Join-Path $env:ProgramData 'diyRAG'        # %ProgramData%\diyRAG
$DataSubDirs  = @('config', 'certs', 'models', 'logs')
$DaemonExe    = 'diyragd.exe'
$CliExe       = 'diyrag.exe'

function Write-Step { param([string]$Msg) Write-Host "==> $Msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$Msg) Write-Host "    $Msg" -ForegroundColor Green }
function Write-Warn { param([string]$Msg) Write-Host "    $Msg" -ForegroundColor Yellow }

# --- 1. Admin check + self-elevation note ------------------------------------
$identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Warning 'This installer must run elevated (Administrator).'
    Write-Host    'Re-run from an elevated PowerShell, or self-elevate with:'
    Write-Host    ('  Start-Process powershell -Verb RunAs -ArgumentList ' +
                   "'-NoProfile -ExecutionPolicy Bypass -File ""$PSCommandPath""'") -ForegroundColor Yellow
    exit 1
}
Write-Ok 'Running with administrative privileges.'

# --- 2. Copy binaries --------------------------------------------------------
Write-Step "Installing binaries to $InstallDir"
$srcDaemon = Join-Path $SourceDir $DaemonExe
$srcCli    = Join-Path $SourceDir $CliExe
foreach ($f in @($srcDaemon, $srcCli)) {
    if (-not (Test-Path $f)) {
        throw "Required binary not found: $f  (build the workspace, or pass -SourceDir)"
    }
}
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
# Stop the service first if it is running so we can replace a locked binary.
$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($existing -and $existing.Status -eq 'Running') {
    Write-Warn 'Service is running; stopping it to update binaries.'
    Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
    (Get-Service $ServiceName).WaitForStatus('Stopped', '00:00:30')
}
Copy-Item -Path $srcDaemon -Destination (Join-Path $InstallDir $DaemonExe) -Force
Copy-Item -Path $srcCli    -Destination (Join-Path $InstallDir $CliExe)    -Force
$DaemonPath = Join-Path $InstallDir $DaemonExe
$CliPath    = Join-Path $InstallDir $CliExe
Write-Ok "Copied $DaemonExe and $CliExe."

# --- 3. ProgramData dirs with restrictive ACLs -------------------------------
# Layout: %ProgramData%\diyRAG\{config,certs,models,logs}  (§16b.2)
# ACL policy (§12.8): only Administrators (Full), SYSTEM (Full), and the service
# account (Modify) may touch this tree. Inheritance is DISABLED and inherited
# ACEs removed, so 'Users' cannot read certs/keys. This blocks the writable-path
# privesc class called out in §22 #13.
Write-Step "Creating data tree with restrictive ACLs under $DataDir"
if (-not (Test-Path $DataDir)) { New-Item -ItemType Directory -Path $DataDir -Force | Out-Null }
foreach ($sub in $DataSubDirs) {
    $p = Join-Path $DataDir $sub
    if (-not (Test-Path $p)) { New-Item -ItemType Directory -Path $p -Force | Out-Null }
}

function Set-LockedAcl {
    param([string]$Path, [string]$ServiceAccount)

    $acl = Get-Acl $Path
    # Disable inheritance and strip inherited ACEs (do NOT copy them).
    $acl.SetAccessRuleProtection($true, $false)
    # Clear any explicit rules so we start from a clean, deny-by-default state.
    foreach ($rule in @($acl.Access)) { [void]$acl.RemoveAccessRule($rule) }

    $inherit = [System.Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'
    $prop    = [System.Security.AccessControl.PropagationFlags]::None
    $allow   = [System.Security.AccessControl.AccessControlType]::Allow

    $rules = @(
        # Administrators — Full
        (New-Object System.Security.AccessControl.FileSystemAccessRule(
            (New-Object System.Security.Principal.SecurityIdentifier(
                [System.Security.Principal.WellKnownSidType]::BuiltinAdministratorsSid, $null)),
            'FullControl', $inherit, $prop, $allow)),
        # SYSTEM — Full
        (New-Object System.Security.AccessControl.FileSystemAccessRule(
            (New-Object System.Security.Principal.SecurityIdentifier(
                [System.Security.Principal.WellKnownSidType]::LocalSystemSid, $null)),
            'FullControl', $inherit, $prop, $allow)),
        # Service account — Modify (read/write its own state, not change ACLs)
        (New-Object System.Security.AccessControl.FileSystemAccessRule(
            $ServiceAccount, 'Modify', $inherit, $prop, $allow))
    )
    foreach ($r in $rules) { $acl.AddAccessRule($r) }
    Set-Acl -Path $Path -AclObject $acl
}

try {
    Set-LockedAcl -Path $DataDir -ServiceAccount $ServiceAccount
    foreach ($sub in $DataSubDirs) {
        Set-LockedAcl -Path (Join-Path $DataDir $sub) -ServiceAccount $ServiceAccount
    }
    Write-Ok "ACLs locked to Administrators, SYSTEM, and '$ServiceAccount'."
} catch {
    Write-Warn "Could not fully apply ACLs for '$ServiceAccount': $($_.Exception.Message)"
    Write-Warn 'If the account is a managed virtual account it may not resolve until first start;'
    Write-Warn 're-run this installer after the service has run once, or grant ACLs manually.'
}

# Seed a default config if none exists (placeholders only; NO secrets — §12.1).
$cfgFile = Join-Path $DataDir 'config\diyrag.env'
if (-not (Test-Path $cfgFile)) {
    @"
# diyRAG Windows config (placeholders only; NO secrets here — §12.1, §19).
# Real secrets via DPAPI / a secret store, never committed.
DIYRAG_ENV=production
DIYRAG_DATA_DIR=$DataDir
DIYRAG_CONFIG_DIR=$DataDir\config
DIYRAG_CERTS_DIR=$DataDir\certs
DIYRAG_MODEL_CACHE_DIR=$DataDir\models
DIYRAGD_MODE=$Mode
RUST_LOG=info,diyrag=info
DIYRAG_LOG_FORMAT=json
# Rust-native inference is the ONLY GPU path on Windows (§16, GPU.md).
EMBED_BACKEND=rust-native
RERANK_BACKEND=rust-native
LLM_BACKEND=rust-llm
ORT_EXECUTION_PROVIDER=cuda
"@ | Set-Content -Path $cfgFile -Encoding UTF8
    Write-Ok "Wrote default config: $cfgFile"
}

# --- 4. Register the service -------------------------------------------------
$binArgs = "service --mode $Mode"

if (-not $UseScFallback) {
    # ----- PREFERRED PATH: the diyrag CLI wraps the SCM (§16b.2) -------------
    Write-Step 'Registering service via the diyrag CLI (preferred)'
    Write-Host  "    & `"$CliPath`" service install --mode $Mode --auto-start --account `"$ServiceAccount`""
    if ($PSCmdlet.ShouldProcess($ServiceName, 'install via diyrag CLI')) {
        & $CliPath service install --mode $Mode --auto-start --account $ServiceAccount
        if ($LASTEXITCODE -ne 0) {
            Write-Warn "CLI install returned $LASTEXITCODE; falling back to sc.exe."
            $UseScFallback = $true
        } else {
            Write-Ok 'Service registered via CLI (StartType=AutoStart).'
        }
    }
}

if ($UseScFallback) {
    # ----- RAW SCM FALLBACK / REFERENCE (§16b.2) ----------------------------
    # The exact equivalent the CLI performs. `start= auto` makes the service
    # start on every device restart; the failure actions satisfy §14 service-
    # level recovery. NOTE the required SPACE after each `=` in sc.exe syntax,
    # and the quoting of the binPath so the path is not treated as unquoted
    # (unquoted-service-path privesc, §22 #13).
    Write-Step 'Registering service via raw sc.exe (fallback)'
    $binPath = "`"$DaemonPath`" $binArgs"

    if ($PSCmdlet.ShouldProcess($ServiceName, 'sc.exe create')) {
        if ($existing) {
            # Idempotent: reconcile config of an existing service.
            & sc.exe config $ServiceName binPath= $binPath start= auto obj= $ServiceAccount | Out-Null
        } else {
            & sc.exe create $ServiceName binPath= $binPath start= auto obj= $ServiceAccount DisplayName= $DisplayName | Out-Null
        }
        & sc.exe description $ServiceName "$DisplayName" | Out-Null
        # Crash recovery: restart after 5s / 10s / 30s; reset the failure
        # counter once per day (§14, §16b.2).
        & sc.exe failure $ServiceName reset= 86400 actions= restart/5000/restart/10000/restart/30000 | Out-Null
        Write-Ok 'Service registered via sc.exe (start= auto, recovery actions set).'
    }
}

# The literal raw-SCM commands, for copy/paste reference (commented):
#
#   sc.exe create diyRAG binPath= "C:\Program Files\diyRAG\diyragd.exe service --mode all-in-one" start= auto obj= "NT SERVICE\diyRAG"
#   sc.exe description diyRAG "diyRAG Retrieval-Augmented Generation Service"
#   sc.exe failure diyRAG reset= 86400 actions= restart/5000/restart/10000/restart/30000
#   sc.exe start  diyRAG

# --- 5. Confirm auto-start + offer to start now ------------------------------
Write-Step 'Verifying boot autostart'
$svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if (-not $svc) { throw "Service '$ServiceName' was not created." }

# Some PS versions need CIM to read StartType reliably.
$startMode = (Get-CimInstance Win32_Service -Filter "Name='$ServiceName'").StartMode
if ($startMode -ne 'Auto') {
    Write-Warn "StartMode is '$startMode'; forcing Automatic so it starts on every reboot."
    Set-Service -Name $ServiceName -StartupType Automatic
}
Write-Ok "Service '$ServiceName' is set to start automatically on boot."

Write-Step 'Starting the service'
if ($PSCmdlet.ShouldProcess($ServiceName, 'start')) {
    # Prefer the CLI for symmetry with the documented control surface.
    & $CliPath service start 2>$null
    if ($LASTEXITCODE -ne 0) { Start-Service -Name $ServiceName }
    (Get-Service $ServiceName).WaitForStatus('Running', '00:00:30')
    Write-Ok 'Service is Running.'
}

Write-Host ''
Write-Host 'diyRAG installed.' -ForegroundColor Green
Write-Host 'Manage it from a terminal:' -ForegroundColor Green
Write-Host '    diyrag service status'
Write-Host '    diyrag service stop'
Write-Host '    diyrag service start'
Write-Host ''
Write-Host 'It will auto-start on every reboot. See README.md for the after-reboot checklist.'
