$ErrorActionPreference = 'Stop'
$spicetify = Join-Path $env:LOCALAPPDATA 'spicetify\spicetify.exe'
$source = Join-Path $PSScriptRoot 'wdock-lyrics-plus.js'
$destination = Join-Path $env:LOCALAPPDATA 'spicetify\Extensions\wdock-lyrics-plus.js'
if (-not (Test-Path -LiteralPath $spicetify)) { throw 'Spicetify was not found in LOCALAPPDATA.' }
if (-not (Test-Path -LiteralPath $source)) { throw 'The WDock lyrics bridge file is missing.' }
if (Test-Path -LiteralPath $destination) {
    $sourceBytes = [Convert]::ToBase64String([IO.File]::ReadAllBytes($source))
    $installedBytes = [Convert]::ToBase64String([IO.File]::ReadAllBytes($destination))
    if ($sourceBytes -ne $installedBytes) {
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $hash = [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($destination))).Replace('-', '').ToLowerInvariant() }
        finally { $sha.Dispose() }
        $knownVersions = @(
            'b9268ed0a3455c1e63dc419401c122f6e4c55e94fd63d9a47b6f5c68b2dcd3b7',
            'd19a1634799b409e70cdf62962ffbf1f0c203f710244df1281293aa6e5650179',
            'dcdfb134c964ba9e053617ef8ce5bb24eb0a0107f2c8142c0a5967f5a437610d',
            '8f67a3e4bb87766691a506a7fee3aad172a26cc7114060e3bd14ba6435d40c31',
            '45220215e37d5b72df26f6a3f4d1b686d989366554e0920b5125e59a0026a202',
            '53629feda79cd55027c3706834aeb536ccbeaa8f7cad9efdfbacf703700f517c',
            '0a13bc88af28d5b0283f8351595ae76bc399214204e9829b8dd6fc56f5d19fea',
            '4501052f64d0af7deb05e44e6b421458f64dd6a5083b1b7977980f4ea908bc6e',
            'f69a439c2d9bad3f4392907a9a11d646433832f1eb1374aa0556a369a3a5f06e',
            '6df970c560c837573c9548533e482c8dac07dd7ec4b5f9a2043b37a4e2b3451f',
            '1b17f2861b1e56dd4b037a135cbe3de107707678a564fc8ef6053b64c6e3ff4d',
            'e801ab6fe7355820a0ff63eba6122879711a6e32ec2a97122b5b61fd6290519e'
        )
        if ($hash -notin $knownVersions) {
            throw "An unrecognized file already exists at $destination. Inspect it before replacing it."
        }
        Copy-Item -LiteralPath $source -Destination $destination -Force
    }
} else {
    $extensionDir = Split-Path -Parent $destination
    New-Item -ItemType Directory -Path $extensionDir -Force | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}
$configured = & $spicetify config extensions
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect Spicetify configuration.' }
if (-not (($configured -join "`n") -match '(?i)(^|[|\s])wdock-lyrics-plus\.js([|\s]|$)')) {
    & $spicetify config extensions wdock-lyrics-plus.js
    if ($LASTEXITCODE -ne 0) { throw 'Could not add the bridge to Spicetify configuration.' }
}
& $spicetify -n apply
if ($LASTEXITCODE -ne 0) { throw 'Spicetify could not apply the extension.' }
Write-Output 'Bridge installed. Restart Spotify and run the newly built WDock to use it.'
