$ErrorActionPreference = 'Continue'

Write-Host "=== 1. Service config + last error ==="
Get-Service vmicguestinterface | Select-Object Name,Status,StartType,DependentServices | Format-List

Write-Host ""
Write-Host "=== 2. Recent Service Control Manager events ==="
Get-WinEvent -FilterHashtable @{LogName='System';ProviderName='Service Control Manager'} -MaxEvents 8 -ErrorAction SilentlyContinue |
    Select-Object TimeCreated,Id,LevelDisplayName,Message |
    Format-List

Write-Host ""
Write-Host "=== 3. Hyper-V / VMBus PnP devices ==="
Get-PnpDevice | Where-Object { $_.FriendlyName -like '*Hyper-V*' -or $_.FriendlyName -like '*VMBus*' -or $_.FriendlyName -like '*Virtualization*' } |
    Select-Object FriendlyName,Status,Class |
    Format-Table -AutoSize

Write-Host ""
Write-Host "=== 4. Integration services registry (GuestCommunicationServices) ==="
$base = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices'
if (Test-Path $base) {
    Get-ChildItem $base | ForEach-Object {
        $props = Get-ItemProperty $_.PSPath
        [PSCustomObject]@{ GUID = $_.PSChildName; ElementName = $props.ElementName }
    } | Format-Table -AutoSize
} else {
    Write-Host "  (registry key not found - integration services may not be offered by host)"
}

Write-Host ""
Write-Host "=== 5. Am I actually a Hyper-V guest? ==="
$wmi = Get-CimInstance -ClassName Win32_ComputerSystem -Namespace root\cimv2 -ErrorAction SilentlyContinue
Write-Host "  Manufacturer: $($wmi.Manufacturer)"
Write-Host "  Model: $($wmi.Model)"
$guestParams = 'HKLM:\SOFTWARE\Microsoft\Virtual Machine\Guest\Parameters'
if (Test-Path $guestParams) {
    $p = Get-ItemProperty $guestParams
    Write-Host "  VM GUID:   $($p.VirtualMachineId)"
    Write-Host "  Host name: $($p.HostName)"
} else {
    Write-Host "  (Guest\Parameters key missing - not a Hyper-V guest, or integration services off)"
}

