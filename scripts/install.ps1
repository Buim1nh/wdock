# Installs wdock for your user account: it starts at every sign-in and runs
# on top of explorer, which stays the Windows shell with its taskbar hidden.
# Needs no admin.
#
# What it does:
#   1. builds the release binary (unless -NoBuild),
#   2. copies it to %LOCALAPPDATA%\Programs\wdock\wdock.exe,
#   3. registers a per-user scheduled task "wdock" that runs "<that exe>" --guard
#      at sign-in, and starts it now unless -NoStart.
#
# A task rather than the HKCU Run key: run from a packaged (MSIX) app's
# terminal, registry writes to HKCU are redirected into the package and
# explorer never sees the Run entry; task registration is not redirected.
#
# Undo with scripts\uninstall.ps1. Ctrl+Alt+Shift+Q quits wdock for the
# session and brings explorer's taskbar back.
param([switch]$NoBuild, [switch]$NoStart)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$built = Join-Path $root 'target\release\wdock.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\wdock'
$exe = Join-Path $dir 'wdock.exe'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$winlogon = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Winlogon'

if (-not $NoBuild) {
    $env:Path += ";$env:USERPROFILE\.cargo\bin"
    Push-Location $root
    try { cargo build --release; if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' } } finally { Pop-Location }
}
if (-not (Test-Path $built)) { throw "missing $built; build first or drop -NoBuild" }

if (Get-Process wdock -ErrorAction SilentlyContinue) {
    throw 'wdock is running; quit it (Ctrl+Alt+Shift+Q) before installing'
}

New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $built $exe -Force
$user = "$env:USERDOMAIN\$env:USERNAME"
$task = @{
    TaskName    = 'wdock'
    Description = 'Starts wdock at sign-in'
    Action      = New-ScheduledTaskAction -Execute $exe -Argument '--guard'
    Trigger     = New-ScheduledTaskTrigger -AtLogOn -User $user
    Principal   = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
    Settings    = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
}
Register-ScheduledTask @task -Force | Out-Null

# Earlier installs used the Run key; two guards must not run.
if ((Get-ItemProperty $run -ErrorAction SilentlyContinue).wdock) {
    Remove-ItemProperty -Path $run -Name wdock
    Write-Host 'Removed the old Run key entry.'
}

# Earlier builds replaced the shell itself; explorer must be the shell now.
$shell = (Get-ItemProperty $winlogon -ErrorAction SilentlyContinue).Shell
if ($shell -and $shell -match 'wdock') {
    Remove-ItemProperty -Path $winlogon -Name Shell
    Write-Host "Removed the old per-user shell value ($shell)."
}

Write-Host "Installed $exe; the 'wdock' scheduled task starts it at every sign-in."
if (-not $NoStart) {
    Start-ScheduledTask -TaskName wdock
    Write-Host 'wdock started.'
}
