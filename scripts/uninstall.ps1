# Stops wdock, brings explorer's taskbar (and any windows hidden in other
# workspaces) back, and removes the sign-in task install.ps1 registered
# (and the Run key entry older installs used).
# -RemoveFiles also deletes the installed binary; -RemoveData deletes logs,
# caches and state in %LOCALAPPDATA%\wdock. Your config in %APPDATA%\wdock is
# kept either way.
param([switch]$RemoveFiles, [switch]$RemoveData)
$ErrorActionPreference = 'Stop'

$dir = Join-Path $env:LOCALAPPDATA 'Programs\wdock'
$exe = Join-Path $dir 'wdock.exe'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$winlogon = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Winlogon'

if (Get-ScheduledTask -TaskName wdock -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName wdock -Confirm:$false
    Write-Host 'Removed the wdock sign-in task.'
}
if ((Get-ItemProperty $run -ErrorAction SilentlyContinue).wdock) {
    Remove-ItemProperty -Path $run -Name wdock
    Write-Host 'Removed wdock from sign-in startup.'
}
$shell = (Get-ItemProperty $winlogon -ErrorAction SilentlyContinue).Shell
if ($shell -and $shell -match 'wdock') {
    Remove-ItemProperty -Path $winlogon -Name Shell
    Write-Host "Removed the old per-user shell value ($shell)."
}

# Stop the guard first so it does not restart the shell, then the shell.
$procs = Get-CimInstance Win32_Process -Filter "Name='wdock.exe'"
$procs | Where-Object CommandLine -match '--guard|--dev(\s|$)' | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
$procs | Where-Object CommandLine -notmatch '--guard|--dev(\s|$)' | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
Start-Sleep -Milliseconds 500

# Killed processes cannot clean up; let a fresh one do it.
$tool = if (Test-Path $exe) { $exe } else { Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\wdock.exe' }
if (Test-Path $tool) {
    Start-Process -FilePath $tool -ArgumentList '--restore-taskbar' -Wait
    Write-Host "Explorer's taskbar restored."
}

if ($RemoveFiles -and (Test-Path $dir)) {
    Remove-Item -Recurse -Force $dir
    Write-Host "Deleted $dir"
}
if ($RemoveData) {
    $data = Join-Path $env:LOCALAPPDATA 'wdock'
    if (Test-Path $data) { Remove-Item -Recurse -Force $data; Write-Host "Deleted $data" }
}
