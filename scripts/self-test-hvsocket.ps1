<#
.SYNOPSIS
  Self-test: connect to qwan-bootstrapd from inside the guest via AF_HYPERV.
.DESCRIPTION
  This proves the hvsocket listener is actually bound and accepting. It does
  NOT drive the full bootstrap handshake (the host-harness does that); it just
  opens a connection, sends HELLO, and reads the response. If this works, the
  host will be able to connect too.
#>
$ErrorActionPreference = 'Continue'

$VmGuid = '995A044D-0B4C-424A-9E8A-05EFCE117BE5'
$ServiceGuid = '3045196F-2A11-4D65-BCC7-3F9EAB09B7ED'

Write-Host "Self-test: connecting to own VM ($VmGuid) service ($ServiceGuid) ..."

# Build the host-harness in debug and run it against ourselves.
# The harness connects, does HELLO/AUTH, and reports.
$Exe = "$PSScriptRoot\..\target\debug\host-harness.exe"
if (-not (Test-Path $Exe)) {
    Write-Host "host-harness.exe not found. Building ..."
    cargo build -p qwanban-integration --bin host-harness 2>&1 | Out-Host
}

& $Exe --vm-guid $VmGuid --service-guid $ServiceGuid --secret bootstrap-secret
Write-Host "Self-test exit code: $LASTEXITCODE"
