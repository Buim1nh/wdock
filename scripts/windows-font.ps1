# Uses a font for Windows' own UI as well as wdock's, or undoes that.
#
#   windows-font.ps1 -Family 'SF Pro'           # install if needed, then substitute
#   windows-font.ps1 -Family auto               # SF Pro, else Inter
#   windows-font.ps1 -Family 'Inter' -FontDir D:\fonts
#   windows-font.ps1 -Restore                   # put the original fonts back
#
# Needs administrator rights (the substitution is machine-wide). Sign out and
# back in for every app to pick it up. When -Family is not installed, every
# .ttf/.otf in -FontDir is installed first; wdock passes its fonts folder
# (%LOCALAPPDATA%\wdock\fonts), where install.ps1 copies the repo's fonts\.
#
# Only FontSubstitutes is changed; Segoe UI's own files stay registered, so
# -Restore (or deleting the values by hand) always brings the original back.
param(
    [string]$Family,
    [string]$FontDir = (Join-Path $env:LOCALAPPDATA 'wdock\fonts'),
    [switch]$Restore
)
$ErrorActionPreference = 'Stop'

$substitutes = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\FontSubstitutes'
$backup = 'HKLM:\SOFTWARE\wdock\FontSubstitutesBackup'
$fontsKey = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts'
# Families Windows 11 draws its interface with.
$targets = @('Segoe UI', 'Segoe UI Variable', 'Segoe UI Variable Text', 'Segoe UI Variable Display')
$none = '<none>'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script as administrator.'
}

function Test-Font([string]$name) {
    Add-Type -AssemblyName System.Drawing
    $installed = New-Object System.Drawing.Text.InstalledFontCollection
    try { return [bool]($installed.Families | Where-Object { $_.Name -eq $name }) }
    finally { $installed.Dispose() }
}

function Install-Fonts([string]$dir) {
    $files = Get-ChildItem -LiteralPath $dir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -in '.ttf', '.otf' }
    if (-not $files) { return 0 }
    $windowsFonts = Join-Path $env:WINDIR 'Fonts'
    foreach ($file in $files) {
        $dest = Join-Path $windowsFonts $file.Name
        if (-not (Test-Path -LiteralPath $dest)) { Copy-Item -LiteralPath $file.FullName -Destination $dest }
        $kind = if ($file.Extension -eq '.otf') { 'OpenType' } else { 'TrueType' }
        # Windows reads the real family names from the file; this label is cosmetic.
        Set-ItemProperty -Path $fontsKey -Name "$($file.BaseName) ($kind)" -Value $file.Name
    }
    return $files.Count
}

if ($Restore) {
    if (-not (Test-Path $backup)) { Write-Output 'Nothing to restore.'; return }
    $saved = Get-ItemProperty $backup
    foreach ($name in $targets) {
        $old = $saved.$name
        if ($null -eq $old -or $old -eq $none) {
            Remove-ItemProperty -Path $substitutes -Name $name -ErrorAction SilentlyContinue
        } else {
            Set-ItemProperty -Path $substitutes -Name $name -Value $old
        }
    }
    Remove-Item -Path $backup -Force
    Write-Output 'Windows fonts restored. Sign out and back in to apply.'
    return
}

if (-not $Family) { throw 'Pass -Family <font family>, -Family auto, or -Restore.' }
if ($Family -eq 'auto') {
    # Same preference as wdock's font = "auto", plus Inter as an open alternative.
    $preferred = @('SF Pro', 'SF Pro Text', 'SF Pro Display', 'Inter')
    $Family = $preferred | Where-Object { Test-Font $_ } | Select-Object -First 1
    if (-not $Family) {
        $count = Install-Fonts $FontDir
        if ($count -gt 0) { Write-Output "Installed $count font file(s) from $FontDir." }
        $Family = $preferred | Where-Object { Test-Font $_ } | Select-Object -First 1
    }
    if (-not $Family) { throw "No SF Pro or Inter font is installed, and $FontDir provides none." }
}
if (-not (Test-Font $Family)) {
    $count = Install-Fonts $FontDir
    if ($count -eq 0) { throw "Font '$Family' is not installed and $FontDir has no .ttf/.otf files." }
    if (-not (Test-Font $Family)) {
        throw "Installed $count file(s) from $FontDir, but none provides the family '$Family'."
    }
    Write-Output "Installed $count font file(s) from $FontDir."
}

# Back up only once, so applying twice cannot overwrite the true originals.
if (-not (Test-Path $backup)) {
    New-Item -Path $backup -Force | Out-Null
    $current = Get-ItemProperty $substitutes
    foreach ($name in $targets) {
        $value = if ($null -ne $current.$name) { $current.$name } else { $none }
        Set-ItemProperty -Path $backup -Name $name -Value $value
    }
}
foreach ($name in $targets) { Set-ItemProperty -Path $substitutes -Name $name -Value $Family }
Write-Output "Windows will use '$Family'. Sign out and back in to apply."
