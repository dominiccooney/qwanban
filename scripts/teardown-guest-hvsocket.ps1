<#
.SYNOPSIS
  Guest-side teardown for qwanban hvsocket integration.
.DESCRIPTION
  Removes the qwanban service GUID from the guest registry. Optionally stops
  vmicguestinterface (pass -StopService). Must be run elevated.
  Setup: run setup-guest-hvsocket.ps1
.PARAMETER StopService
  Also stop vmicguestinterface after removing the GUID.
#>
param(
    [switch]$StopService
)
$ErrorActionPreference = 'Stop'

$ServiceGuid = '3045196F-2A11-4D65-BCC7-3F9EAB09B7ED'
$RegKey = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\$ServiceGuid"

Write-Host "[1/2] Removing service GUID $ServiceGuid ..."
if (Test-Path $RegKey) {
    try {
        Remove-Item -Path $RegKey -Recurse -Force
        Write-Host "  Removed registry key."
    } catch {
        Write-Host "  FAILED (needs elevation): $_" -ForegroundColor Red
        exit 1
    }
} else {
    Write-Host "  Not registered (nothing to remove)."
}

Write-Host "[2/2] vmicguestinterface ..."
if ($StopService) {
    try {
        Stop-Service vmicguestinterface -Force
        Write-Host "  Stopped."
    } catch {
        Write-Host "  Could not stop: $_" -ForegroundColor Yellow
    }
} else {
    Write-Host "  Left running (pass -StopService to also stop it)."
}

Write-Host ""
Write-Host "Guest teardown complete." -ForegroundColor Green
