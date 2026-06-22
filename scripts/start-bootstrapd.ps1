<#
.SYNOPSIS
  Launch qwan-bootstrapd persistently in the background (guest-side, TCP).
.DESCRIPTION
  Starts the TCP stub-loader daemon. Binds a TCP port on the private vSwitch
  and stays up, accepting host connections one at a time.
  Stop with stop-bootstrapd.ps1.
.PARAMETER BindAddr
  The bind address. Default: 0.0.0.0:7474
.PARAMETER WorkDir
  Where the stub writes pushed files + launches processes. Default: TEMP.
.PARAMETER Secret
  The bootstrap secret the host must present. Default: bootstrap-secret.
#>
param(
    [string]$BindAddr = '0.0.0.0:7474',
    [string]$WorkDir = "$env:TEMP\qwan-bootstrapd-work",
    [string]$Secret = 'bootstrap-secret'
)
$ErrorActionPreference = 'Stop'

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
Write-Host "  BindAddr:  $BindAddr"
Write-Host "  WorkDir:   $WorkDir"
Write-Host "  Secret:    $Secret"

$env:RUST_LOG = 'qwanban=info'
$proc = Start-Process -FilePath $Exe -ArgumentList @(
    '--bind', $BindAddr,
    '--work-dir', $WorkDir,
    '--secret', $Secret
) -WindowStyle Hidden -PassThru

Start-Sleep -Seconds 2
if ($proc.HasExited) {
    Write-Host "FAILED: process exited (code $($proc.ExitCode))." -ForegroundColor Red
    exit 1
}

Write-Host "Started (PID $($proc.Id)). Listening on $BindAddr." -ForegroundColor Green
Write-Host "Stop with: .\scripts\stop-bootstrapd.ps1"
