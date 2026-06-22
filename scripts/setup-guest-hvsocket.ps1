<#
.SYNOPSIS
  Guest-side setup for qwanban hvsocket integration.
.DESCRIPTION
  Verifies vmicguestinterface is running, registers the qwanban service GUID
  in the guest registry so AF_HYPERV bind() accepts it, and verifies the result.
  Must be run elevated (writes to HKLM).
  Teardown: run teardown-guest-hvsocket.ps1
.NOTES
  Service GUID: 3045196F-2A11-4D65-BCC7-3F9EAB09B7ED (qwan-bootstrapd)
  Prerequisite: on the host, run setup-host-vmic.ps1 to enable "Guest Service
  Interface" on this VM first (the VMBus channel must be open or the service
  cannot start).
#>
$ErrorActionPreference = 'Stop'

$ServiceGuid = '3045196F-2A11-4D65-BCC7-3F9EAB09B7ED'
$RegKey = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\$ServiceGuid"

# --- Step 1: Ensure vmicguestinterface is running ---
Write-Host "[1/3] vmicguestinterface service ..."
$svc = Get-Service vmicguestinterface -ErrorAction SilentlyContinue
if (-not $svc) {
    Write-Host "  ERROR: vmicguestinterface not found. Is this a Hyper-V guest?" -ForegroundColor Red
    exit 1
}
if ($svc.Status -ne 'Running') {
    try {
        Start-Service vmicguestinterface
        Write-Host "  Started."
    } catch {
        Write-Host "  FAILED to start: $_" -ForegroundColor Red
        Write-Host "  This usually means the host hasn't enabled 'Guest Service" -ForegroundColor Yellow
        Write-Host "  Interface' on this VM. Run scripts/setup-host-vmic.ps1 on the host." -ForegroundColor Yellow
        exit 1
    }
} else {
    Write-Host "  Already running."
}

# --- Step 2: Register the service GUID ---
Write-Host "[2/3] Registering service GUID $ServiceGuid ..."
if (Test-Path $RegKey) {
    Write-Host "  Already registered."
} else {
    try {
        New-Item -Path $RegKey -Force | Out-Null
        Write-Host "  Created registry key."
    } catch {
        Write-Host "  FAILED (needs elevation): $_" -ForegroundColor Red
        Write-Host "  Re-run this script in an elevated PowerShell." -ForegroundColor Yellow
        exit 1
    }
}
Set-ItemProperty -Path $RegKey -Name 'ElementName' -Value 'qwan-bootstrapd'
Write-Host "  ElementName set to 'qwan-bootstrapd'."

# --- Step 3: Verify ---
Write-Host "[3/3] Verification ..."
Get-Service vmicguestinterface | Select-Object Name,Status,StartType | Format-Table -AutoSize
Get-PnpDevice | Where-Object { $_.FriendlyName -like '*Guest Service Interface*' } |
    Select-Object FriendlyName,Status | Format-Table -AutoSize
$props = Get-ItemProperty $RegKey
Write-Host "Service GUID  : $ServiceGuid"
Write-Host "ElementName   : $($props.ElementName)"
Write-Host ""
Write-Host "Guest setup complete. Next: build + run qwan-bootstrapd (see runbook Step 2-3)." -ForegroundColor Green
