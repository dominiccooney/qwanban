<#
.SYNOPSIS
  Host-side teardown: disable "Guest Service Interface" on a Hyper-V VM.
.DESCRIPTION
  Disables the "Guest Service Interface" integration service on the target VM,
  closing the VMBus channel. The guest's vmicguestinterface service will no
  longer be able to start. AF_HYPERV sockets in the guest will stop working.
  Must be run elevated on the Hyper-V host.
  Setup: run setup-host-vmic.ps1
.PARAMETER VmName
  The name of the guest VM. Defaults to the dev VM.
#>
param(
    [string]$VmName = 'Windows 11 dev environment'
)
$ErrorActionPreference = 'Stop'

Write-Host "Disabling 'Guest Service Interface' on VM '$VmName' ..."
$svc = Get-VMIntegrationService -VMName $VmName -Name 'Guest Service Interface' -ErrorAction SilentlyContinue
if (-not $svc) {
    Write-Host "  Not found on this VM (nothing to tear down)." -ForegroundColor Yellow
    exit 0
}
if (-not $svc.Enabled) {
    Write-Host "  Already disabled."
} else {
    Disable-VMIntegrationService -VMName $VmName -Name 'Guest Service Interface'
    Write-Host "  Disabled."
}
Write-Host ""
Write-Host "Host teardown complete." -ForegroundColor Green
