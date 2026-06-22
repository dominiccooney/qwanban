$ErrorActionPreference = 'Continue'

Write-Host "=== vmicguestinterface status ==="
Get-Service vmicguestinterface | Select-Object Name,Status,StartType | Format-Table -AutoSize

Write-Host ""
Write-Host "=== Guest Service Interface PnP device ==="
Get-PnpDevice | Where-Object { $_.FriendlyName -like '*Guest Service Interface*' } |
    Select-Object FriendlyName,Status | Format-Table -AutoSize

Write-Host ""
Write-Host "=== Register qwanban service GUID ==="
$key = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\3045196F-2A11-4D65-BCC7-3F9EAB09B7ED'
if (-not (Test-Path $key)) {
    try {
        New-Item -Path $key -Force -ErrorAction Stop | Out-Null
        Set-ItemProperty -Path $key -Name 'ElementName' -Value 'qwan-bootstrapd' -ErrorAction Stop
        Write-Host "  Registered OK"
    } catch {
        Write-Host "  FAILED (needs elevation): $_"
        Write-Host "  Run this in an elevated shell:"
        Write-Host "    New-Item -Path '$key' -Force | Out-Null"
        Write-Host "    Set-ItemProperty -Path '$key' -Name 'ElementName' -Value 'qwan-bootstrapd'"
    }
} else {
    Write-Host "  Already registered"
}
Get-ItemProperty $key -ErrorAction SilentlyContinue | Select-Object ElementName | Format-Table -AutoSize
