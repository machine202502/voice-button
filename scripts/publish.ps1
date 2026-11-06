# Build packages and attach them to a GitHub release. Requires authenticated gh.
param([string]$Tag, [switch]$Draft)

$ErrorActionPreference = 'Stop'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $Root
$cargo = Get-Content -Raw -Path (Join-Path $Root 'Cargo.toml')
if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Could not read Cargo.toml version' }
$Version = $Matches[1]
if (-not $Tag) { $Tag = "v$Version" }

& (Join-Path $PSScriptRoot 'package.ps1') -Version $Version
if ($LASTEXITCODE -ne 0) { throw 'package.ps1 failed' }
$portable = Join-Path $Root "dist\voice-button-$Version-windows-x64.zip"
$setup = Join-Path $Root "dist\voice-button-$Version-windows-x64-setup.exe"
foreach ($path in @($portable, $setup)) {
    if (-not (Test-Path $path)) { throw "Missing release file: $path" }
}

$notes = @"
Windows 10/11 x64.

- ``voice-button-$Version-windows-x64.zip`` is portable. Unzip it and run ``voice-button.exe``; keep ``voice-button-ui.exe`` beside it.
- ``voice-button-$Version-windows-x64-setup.exe`` installs both executables for all users.

Voice Button source is available under the MIT License. LICENSE, NOTICE, AISOFTWARE, and THIRD-PARTY.txt are included in both packages. Speech models are downloaded separately by the user.
Source for this tag: https://github.com/machine202502/voice-button/tree/$Tag
"@
$ghArgs = @('release', 'create', $Tag, '--title', "Voice Button $Version", '--notes', $notes, $portable, $setup)
if ($Draft) { $ghArgs += '--draft' }
gh @ghArgs
if ($LASTEXITCODE -ne 0) { throw 'gh release create failed' }
