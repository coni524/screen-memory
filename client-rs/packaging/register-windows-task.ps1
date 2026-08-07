# Registers screen-memory with Task Scheduler (runs in the notification area at logon).
# Usage: powershell -ExecutionPolicy Bypass -File packaging\register-windows-task.ps1 [path to exe]
#
# Registering the exe directly leaves a console window open, so launch it hidden
# through PowerShell instead (the registered PowerShell itself flashes briefly at logon).
param(
    [string]$ExePath = (Join-Path $PSScriptRoot "..\target\release\screen-memory.exe")
)
$ExePath = (Resolve-Path $ExePath).Path

$action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument (
    "-NoProfile -WindowStyle Hidden -Command Start-Process -WindowStyle Hidden -FilePath '$ExePath' -ArgumentList tray"
)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
# Without ExecutionTimeLimit set to 0, Task Scheduler kills the task after the default 3 days
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero)
Register-ScheduledTask -TaskName "screen-memory" -Action $action -Trigger $trigger `
    -Settings $settings -Force | Out-Null

Write-Host "Registered the screen-memory task. It starts from the next logon on."
Write-Host "To start it now: Start-ScheduledTask -TaskName screen-memory"
