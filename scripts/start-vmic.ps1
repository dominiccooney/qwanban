$ErrorActionPreference = 'Stop'
$action = New-ScheduledTaskAction -Execute 'net.exe' -Argument 'start vmicguestinterface'
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -RunLevel Highest
$task = New-ScheduledTask -Action $action -Principal $principal
Register-ScheduledTask -TaskName 'qwan-start-vmic' -InputObject $task -Force | Out-Null
Start-ScheduledTask -TaskName 'qwan-start-vmic'
Start-Sleep -Seconds 3
Get-Service vmicguestinterface | Select-Object Name,Status
