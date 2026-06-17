<#
.SYNOPSIS
    diyRAG Windows Service uninstaller (MASTER_BUILD_SPEC.md §16b.2).

.DESCRIPTION
    Stops and deletes the `diyRAG` Windows Service and removes the installed
    binaries. By DEFAULT it KEEPS the data tree under %ProgramData%\diyRAG so an
    accidental uninstall never destroys ingested content/config (mirrors the
    "removal never silently destroys data" guarantee, §1 / §6.6).

    Pass -RemoveData to also delete %ProgramData%\diyRAG (irreversible —
    requires -Force or an interactive confirmation).

.PARAMETER InstallDir
    Install dir to remove (default: C:\Program Files\diyRAG).

.PARAMETER RemoveData
    Also delete %ProgramData%\diyRAG (config, certs, models, logs). OFF by default.

.PARAMETER Force
    Skip the confirmation prompt for data removal.

.EXAMPLE
    .\uninstall.ps1                 # stop+delete service, KEEP data

.EXAMPLE
    .\uninstall.ps1 -RemoveData -Force   # also wipe %ProgramData%\diyRAG

.NOTES
    Requires admin.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$InstallDir = (Join-Path $env:ProgramFiles 'diyRAG'),
    [switch]$RemoveData,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ServiceName = 'diyRAG'
$DataDir     = Join-Path $env:ProgramData 'diyRAG'
$CliPath     = Join-Path $InstallDir 'diyrag.exe'

function Write-Step { param([string]$Msg) Write-Host "==> $Msg" -ForegroundColor Cyan }
function Write-Ok   { param([string]$Msg) Write-Host "    $Msg" -ForegroundColor Green }
function Write-Warn { param([string]$Msg) Write-Host "    $Msg" -ForegroundColor Yellow }

# --- Admin check + self-elevation note ---------------------------------------
$identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Warning 'This uninstaller must run elevated (Administrator).'
    Write-Host    'Self-elevate with:'
    Write-Host    ('  Start-Process powershell -Verb RunAs -ArgumentList ' +
                   "'-NoProfile -ExecutionPolicy Bypass -File ""$PSCommandPath""'") -ForegroundColor Yellow
    exit 1
}

# --- 1. Stop the service -----------------------------------------------------
$svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($svc) {
    if ($svc.Status -ne 'Stopped') {
        Write-Step 'Stopping the service (drains in-flight work, §16b.2)'
        if ($PSCmdlet.ShouldProcess($ServiceName, 'stop')) {
            # Prefer the CLI; fall back to Stop-Service.
            if (Test-Path $CliPath) { & $CliPath service stop 2>$null }
            if ((Get-Service $ServiceName).Status -ne 'Stopped') {
                Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
            }
            try { (Get-Service $ServiceName).WaitForStatus('Stopped', '00:00:30') } catch {}
            Write-Ok 'Service stopped.'
        }
    }

    # --- 2. Delete the service ------------------------------------------------
    Write-Step 'Deleting the service'
    if ($PSCmdlet.ShouldProcess($ServiceName, 'delete')) {
        $deleted = $false
        if (Test-Path $CliPath) {
            & $CliPath service uninstall 2>$null
            if ($LASTEXITCODE -eq 0) { $deleted = $true }
        }
        if (-not $deleted) {
            # Raw SCM fallback / reference: sc.exe delete diyRAG
            & sc.exe delete $ServiceName | Out-Null
        }
        Write-Ok "Service '$ServiceName' deleted."
    }
} else {
    Write-Warn "Service '$ServiceName' is not installed; nothing to stop/delete."
}

# --- 3. Remove binaries ------------------------------------------------------
if (Test-Path $InstallDir) {
    Write-Step "Removing binaries from $InstallDir"
    if ($PSCmdlet.ShouldProcess($InstallDir, 'remove')) {
        Remove-Item -Path $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Ok 'Binaries removed.'
    }
}

# --- 4. Data: keep (default) or remove (opt-in) ------------------------------
if ($RemoveData) {
    if (Test-Path $DataDir) {
        $proceed = $Force
        if (-not $proceed) {
            $ans = Read-Host "Delete ALL diyRAG data under '$DataDir'? This is irreversible. (yes/no)"
            $proceed = ($ans -eq 'yes')
        }
        if ($proceed) {
            Write-Step "Removing data tree $DataDir"
            if ($PSCmdlet.ShouldProcess($DataDir, 'remove data')) {
                Remove-Item -Path $DataDir -Recurse -Force -ErrorAction SilentlyContinue
                Write-Ok 'Data removed.'
            }
        } else {
            Write-Warn 'Data removal cancelled; data kept.'
        }
    }
} else {
    Write-Ok "Data kept at '$DataDir' (pass -RemoveData to delete it)."
}

Write-Host ''
Write-Host 'diyRAG uninstalled.' -ForegroundColor Green
