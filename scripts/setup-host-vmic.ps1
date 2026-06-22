<#
.SYNOPSIS
  Host-side setup: enable "Guest Service Interface" on a Hyper-V VM.
.DESCRIPTION
  Enables the "Guest Service Interface" integration service on the target VM.
  This opens the VMBus channel that backs AF_HYPERV (hvsocket) sockets inside
  the guest. Without this, the guest's vmicguestinterface service cannot start
  and AF_HYPERV bind() will fail with WSAEACCES.
  Must be run elevated on the Hyper-V host.
  Teardown: run teardown-host-vmic.ps1
.PARAMETER VmName
  The name of the guest VM to enable the service on. Defaults to the dev VM.
#>
param(
    [string]$VmName = 'Windows 11 dev environment'
)
$ErrorActionPreference = 'Stop'

Write-Host "Host: $env:COMPUTERNAME"
Write-Host "VM:   $VmName"
Write-Host ""

# --- Step 1: Verify the VM exists ---
Write-Host "[1/3] Verifying VM exists ..."
$vm = Get-VM -Name $VmName -ErrorAction SilentlyContinue
if (-not $vm) {
    Write-Host "  ERROR: VM '$VmName' not found on this host." -ForegroundColor Red
    Write-Host "  Available VMs:" -ForegroundColor Yellow
    Get-VM | Select-Object Name,State | Format-Table -AutoSize
    exit 1
}
Write-Host "  Found: $($vm.Name) [$($vm.State)]"

# --- Step 2: Enable Guest Service Interface ---
Write-Host "[2/3] Enabling 'Guest Service Interface' integration service ..."
$svc = Get-VMIntegrationService -VMName $VmName -Name 'Guest Service Interface' -ErrorAction SilentlyContinue
if (-not $svc) {
    Write-Host "  ERROR: 'Guest Service Interface' not offered to this VM." -ForegroundColor Red
    Write-Host "  All integration services on this VM:" -ForegroundColor Yellow
    Get-VMIntegrationService -VMName $VmName | Select-Object Name,Enabled | Format-Table -AutoSize
    exit 1
}
if ($svc.Enabled) {
    Write-Host "  Already enabled."
} else {
    Enable-VMIntegrationService -VMName $VmName -Name 'Guest Service Interface'
    Write-Host "  Enabled."
}

# --- Step 3: Verify + show all integration services ---
Write-Host "[3/3] Verification ..."
Get-VMIntegrationService -VMName $VmName |
    Select-Object Name,Enabled |
    Format-Table -AutoSize

Write-Host ""
Write-Host "Host setup complete. Next: run setup-guest-hvsocket.ps1 inside the guest VM." -ForegroundColor Green
