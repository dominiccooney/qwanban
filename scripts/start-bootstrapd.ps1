<#
.SYNOPSIS
  Launch qwan-bootstrapd persistently in the background (guest-side).
.DESCRIPTION
  Starts the hvsocket stub-loader daemon bound to the qwanban service GUID.
  It runs hidden in the background and stays up, accepting host connections.
  Stop with stop-bootstrapd.ps1.
.PARAMETER WorkDir
  Where the stub writes pushed files + launches processes. Default: TEMP.
.PARAMETER Secret
  The bootstrap secret the host must present. Default: bootstrap-secret.
#>
param(
    [string]$WorkDir = "$env:TEMP\qwan-bootstrapd-work",
    [string]$Secret = 'bootstrap-secret'
)
$ErrorActionPreference = 'Stop'

$ServiceGuid = '3045196F-2A11-4D65-BCC7-3F9EAB09B7ED'
$Exe = "$PSScriptRoot\..\target\debug\qwan-bootstrapd.exe"

if (-not (Test-Path $Exe)) {
    Write-Host "Binary not found at $Exe" -ForegroundColor Red
    Write-Host "Build it first: cargo build -p qwanban-stub --bin qwan-bootstrapd" -ForegroundColor Yellow
    exit 1
}

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# Kill any previous instance
Get-Process qwan-bootstrapd -ErrorAction SilentlyContinue | Stop-Process -Force

Write-Host "Starting qwan-bootstrapd ..."
Write-Host "  Exe:         $Exe"
Write-Host "  WorkDir:     $WorkDir"
Write-Host "  ServiceGUID: $ServiceGuid"

# Start hidden, no redirect (avoids pipe-handle blocking). Tracing output is
# discarded; the daemon blocks on accept() and stays alive.
$env:RUST_LOG = 'qwanban=info'
$proc = Start-Process -FilePath $Exe -ArgumentList @(
    '--service-guid', $ServiceGuid,
    '--work-dir', $WorkDir,
    '--secret', $Secret
) -WindowStyle Hidden -PassThru

Start-Sleep -Seconds 2
if ($proc.HasExited) {
    Write-Host "FAILED: process exited (code $($proc.ExitCode))." -ForegroundColor Red
    exit 1
}

Write-Host "Started (PID $($proc.Id)). Listening for host connections." -ForegroundColor Green
Write-Host "Stop with: .\scripts\stop-bootstrapd.ps1"
