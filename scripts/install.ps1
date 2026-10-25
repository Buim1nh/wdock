# Installs wdock for your user account: it starts at every sign-in and runs
# on top of explorer, which stays the Windows shell with its taskbar hidden.
# Needs no admin.
#
# What it does:
#   1. builds the release binary (unless -NoBuild),
#   2. copies it to %LOCALAPPDATA%\Programs\wdock\wdock.exe,
#   3. adds HKCU\Software\Microsoft\Windows\CurrentVersion\Run\wdock =
#      "<that exe>" --guard, and starts it now unless -NoStart.
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
Set-ItemProperty -Path $run -Name wdock -Value "`"$exe`" --guard"

# Earlier builds replaced the shell itself; explorer must be the shell now.
$shell = (Get-ItemProperty $winlogon -ErrorAction SilentlyContinue).Shell
if ($shell -and $shell -match 'wdock') {
    Remove-ItemProperty -Path $winlogon -Name Shell
    Write-Host "Removed the old per-user shell value ($shell)."
}

Write-Host "Installed $exe; it starts at every sign-in."
if (-not $NoStart) {
    Start-Process -FilePath $exe -ArgumentList '--guard'
    Write-Host 'wdock started.'
}
