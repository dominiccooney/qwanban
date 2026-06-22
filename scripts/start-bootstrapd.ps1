<#
.SYNOPSIS
  Launch qwan-bootstrapd persistently in the background (guest-side).
.DESCRIPTION
  Starts the hvsocket stub-loader daemon bound to the qwanban service GUID.
  It runs in the background and stays up, accepting host connections one at a
  time. Logs go to $env:TEMP\qwan-bootstrapd.log.
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
$LogFile = "$env:TEMP\qwan-bootstrapd.log"
$ErrFile = "$env:TEMP\qwan-bootstrapd.err"

if (-not (Test-Path $Exe)) {
    Write-Host "Binary not found at $Exe" -ForegroundColor Red
    Write-Host "Build it first: cargo build -p qwanban-stub --bin qwan-bootstrapd" -ForegroundColor Yellow
    exit 1
}

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

# Kill any previous instance
Get-Process qwan-bootstrapd -ErrorAction SilentlyContinue | Stop-Process -Force

$argList = @(
    '--service-guid', $ServiceGuid,
    '--work-dir', $WorkDir,
    '--secret', $Secret
)
Write-Host "Starting qwan-bootstrapd ..."
Write-Host "  Exe:        $Exe"
Write-Host "  WorkDir:    $WorkDir"
Write-Host "  Log:        $LogFile"
Write-Host "  ServiceGUID:$ServiceGuid"

$proc = Start-Process -FilePath $Exe -ArgumentList $argList `
    -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $LogFile -RedirectStandardError $ErrFile

Start-Sleep -Seconds 1
if ($proc.HasExited) {
    Write-Host "FAILED: process exited immediately. Log:" -ForegroundColor Red
    Get-Content $LogFile -ErrorAction SilentlyContinue
    exit 1
}

Write-Host "Started (PID $($proc.Id)). Listening for host connections." -ForegroundColor Green
Write-Host "Stop with: .\scripts\stop-bootstrapd.ps1"
