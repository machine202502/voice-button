# Build packages and attach them to a GitHub release. Requires authenticated gh.
param([string]$Tag, [switch]$Draft)

$ErrorActionPreference = 'Stop'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $Root
$cargo = Get-Content -Raw -Path (Join-Path $Root 'Cargo.toml')
if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Could not read Cargo.toml version' }
$Version = $Matches[1]
if (-not $Tag) { $Tag = "v$Version" }

# The release keeps its own layout: artifacts in dist/, next to the checkout.
$env:VB_DIST_DIR = Join-Path $Root 'dist'
& (Join-Path $PSScriptRoot 'package.ps1') -Version $Version
if ($LASTEXITCODE -ne 0) { throw 'package.ps1 failed' }
$portable = Join-Path $Root "dist\voice-button-$Version-windows-x64.zip"
$setup = Join-Path $Root "dist\voice-button-$Version-windows-x64-setup.msi"
$feed = Join-Path $Root "dist\velopack\releases.win.json"
$packages = @(Get-ChildItem -Path (Join-Path $Root 'dist\velopack') -Filter '*.nupkg' -ErrorAction SilentlyContinue | ForEach-Object FullName)
foreach ($path in @($portable, $setup, $feed)) {
    if (-not (Test-Path $path)) { throw "Missing release file: $path" }
}
if ($packages.Count -eq 0) { throw 'No Velopack packages were produced' }

$notes = @"
Windows 10/11 x64.

- ``voice-button-$Version-windows-x64.zip`` is portable. Unzip it and run ``voice-button.exe``; keep ``voice-button-ui.exe`` beside it.
- ``voice-button-$Version-windows-x64-setup.msi`` installs both executables for the current user (no administrator prompt) and installs the built-in updater.
- ``releases.win.json`` and the ``*.nupkg`` assets are the update feed used by the About window.

Voice Button source is available under the MIT License. LICENSE, NOTICE, AISOFTWARE, and THIRD-PARTY.txt are included in both packages. Speech models are downloaded separately by the user.
Source for this tag: https://github.com/machine202502/voice-button/tree/$Tag
"@
$assets = @($portable, $setup, $feed) + $packages
$ghArgs = @('release', 'create', $Tag, '--title', "Voice Button $Version", '--notes', $notes) + $assets
if ($Draft) { $ghArgs += '--draft' }
gh @ghArgs
if ($LASTEXITCODE -ne 0) { throw 'gh release create failed' }
