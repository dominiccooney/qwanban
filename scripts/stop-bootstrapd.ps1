<#
.SYNOPSIS
  Stop the running qwan-bootstrapd daemon (guest-side).
#>
$ErrorActionPreference = 'Continue'
$procs = Get-Process qwan-bootstrapd -ErrorAction SilentlyContinue
if ($procs) {
    $procs | Stop-Process -Force
    Write-Host "Stopped qwan-bootstrapd (PID $($procs.Id -join ', '))." -ForegroundColor Green
} else {
    Write-Host "qwan-bootstrapd is not running."
}
