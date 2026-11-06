# Build both executables and create portable ZIP + Inno Setup installer in dist/.
param([string]$Version)

function Test-ShippedOnWindowsX64 {
    param([string]$Target)
    if (-not $Target) { return $true }
    $t = $Target.ToLowerInvariant()
    if ($t -eq 'cfg(windows)' -or $t -eq 'cfg(target_os = "windows")') { return $true }
    if ($t -eq 'cfg(not(windows))' -or $t -eq 'cfg(unix)' -or $t -eq 'cfg(any())') { return $false }
    foreach ($skip in @('apple', 'android', 'unix', 'linux', 'ios', 'wasm', 'hermit', 'redox', 'dragonfly', 'freebsd', 'gnullvm', 'target_env = "gnu"', 'target_arch = "x86"', 'target_arch = "aarch64"')) {
        if ($t.Contains($skip)) { return $false }
    }
    return ($t.Contains('x86_64') -and $t.Contains('msvc'))
}

function Test-ProcMacroOnly {
    param($Package)
    $kinds = @()
    foreach ($target in @($Package.targets)) {
        foreach ($kind in @($target.kind)) { $kinds += $kind }
    }
    $other = @($kinds | Where-Object { $_ -ne 'proc-macro' })
    return ($kinds.Count -gt 0 -and $other.Count -eq 0)
}

function New-ThirdPartyNotice {
    param([string]$Version)
    $root = Resolve-Path (Join-Path $PSScriptRoot '..')
    $text = New-Object System.Text.StringBuilder
    [void]$text.AppendLine("voice-button $Version")
    [void]$text.AppendLine('')
    [void]$text.AppendLine('Voice Button includes third-party components. This file carries their source, copyright, and license notices. Speech models are downloaded separately by the user and are not bundled.')
    [void]$text.AppendLine('')
    [void]$text.AppendLine("Source for this version: https://github.com/machine202502/voice-button/tree/v$Version")
    [void]$text.AppendLine('')
    foreach ($file in @('ONNX-Runtime-MIT.txt', 'whisper.cpp-MIT.txt', 'sherpa-onnx-Apache-2.0.txt')) {
        [void]$text.AppendLine("==== $file ====")
        [void]$text.AppendLine('')
        [void]$text.AppendLine([System.IO.File]::ReadAllText((Join-Path $root "licenses\$file")).TrimEnd())
        [void]$text.AppendLine('')
    }
    [void]$text.AppendLine('==== Bundled sound cues ====')
    [void]$text.AppendLine('')
    [void]$text.AppendLine([System.IO.File]::ReadAllText((Join-Path $root 'assets\CREDITS.md')).TrimEnd())
    [void]$text.AppendLine('')
    [void]$text.AppendLine('==== Rust crates ====')
    [void]$text.AppendLine('')

    Push-Location $root
    try { $metaJson = (& cargo metadata --format-version 1 | Out-String) }
    finally { Pop-Location }
    if ($LASTEXITCODE -ne 0 -or -not $metaJson) { throw 'cargo metadata failed' }
    if ($PSVersionTable.PSVersion.Major -ge 6) { $meta = $metaJson | ConvertFrom-Json -Depth 100 }
    else { $meta = $metaJson | ConvertFrom-Json }

    $nodes = @{}
    foreach ($node in $meta.resolve.nodes) { $nodes[$node.id] = $node }
    $pkgs = @{}
    foreach ($pkg in $meta.packages) { $pkgs[$pkg.id] = $pkg }
    $queue = New-Object System.Collections.Generic.Queue[string]
    $queue.Enqueue([string]$meta.resolve.root)
    $seen = @{}
    $crates = @()
    while ($queue.Count -gt 0) {
        $id = $queue.Dequeue()
        if ($seen.ContainsKey($id)) { continue }
        $seen[$id] = $true
        $node = $nodes[$id]
        if ($id -ne $meta.resolve.root -and (Test-ProcMacroOnly $pkgs[$id])) { continue }
        if ($id -ne $meta.resolve.root) { $crates += $pkgs[$id] }
        foreach ($dep in @($node.deps)) {
            $take = $false
            foreach ($kind in @($dep.dep_kinds)) {
                if ($kind.kind -and $kind.kind -ne 'normal') { continue }
                if (Test-ShippedOnWindowsX64 $kind.target) { $take = $true }
            }
            if ($take) { $queue.Enqueue([string]$dep.pkg) }
        }
    }

    foreach ($pkg in ($crates | Sort-Object name, version)) {
        $authors = @($pkg.authors) -join ', '
        if (-not $authors) { $authors = 'not recorded' }
        $repository = $pkg.repository
        if (-not $repository) { $repository = $pkg.homepage }
        if (-not $repository) { $repository = 'not recorded' }
        [void]$text.AppendLine("---- $($pkg.name) $($pkg.version) ----")
        [void]$text.AppendLine("License: $($pkg.license)")
        [void]$text.AppendLine("Authors: $authors")
        [void]$text.AppendLine("Repository: $repository")
        [void]$text.AppendLine('')
        $dir = Split-Path $pkg.manifest_path
        $files = @(Get-ChildItem -File $dir | Where-Object { $_.Name -match '^(?i)(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)' } | Sort-Object Name)
        if ($files.Count -eq 0) {
            [void]$text.AppendLine("This crate shipped no license file. Its declared license is $($pkg.license).")
            [void]$text.AppendLine('')
            continue
        }
        foreach ($file in $files) {
            [void]$text.AppendLine("[$($file.Name)]")
            [void]$text.AppendLine([System.IO.File]::ReadAllText($file.FullName).TrimEnd())
            [void]$text.AppendLine('')
        }
    }
    return $text.ToString().TrimEnd()
}

if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    $Root = Resolve-Path (Join-Path $PSScriptRoot '..')
    # whisper.cpp's Vulkan shader generator builds CMake scratch files with a
    # path so long that MSBuild's file tracker fails under target\release\...
    # Keep the build directory short on Windows.
    if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\vbt' }
    $TargetDir = $env:CARGO_TARGET_DIR
    Set-Location $Root
    if (-not $Version) {
        $cargo = Get-Content -Raw -Path (Join-Path $Root 'Cargo.toml')
        if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Could not read Cargo.toml version' }
        $Version = $Matches[1]
    }

    Write-Host "Building Voice Button $Version"
    cargo build --release --bins
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
    $daemon = Join-Path $TargetDir 'release\voice-button.exe'
    $ui = Join-Path $TargetDir 'release\voice-button-ui.exe'
    foreach ($exe in @($daemon, $ui)) {
        if (-not (Test-Path $exe)) { throw "Missing executable: $exe" }
    }

    $dist = Join-Path $Root 'dist'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    $thirdParty = Join-Path $dist 'THIRD-PARTY.txt'
    Set-Content -Path $thirdParty -Encoding utf8 -Value (New-ThirdPartyNotice -Version $Version)

    $portable = Join-Path $dist "voice-button-$Version-windows-x64.zip"
    if (Test-Path $portable) { Remove-Item $portable -Force }
    $stage = Join-Path $dist 'portable'
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item $daemon (Join-Path $stage 'voice-button.exe')
    Copy-Item $ui (Join-Path $stage 'voice-button-ui.exe')
    foreach ($dll in @('sherpa-onnx-c-api.dll', 'sherpa-onnx-cxx-api.dll', 'onnxruntime.dll')) {
        Copy-Item (Join-Path $TargetDir "release\$dll") (Join-Path $stage $dll)
    }
    foreach ($name in @('LICENSE', 'NOTICE', 'AISOFTWARE')) {
        Copy-Item (Join-Path $Root $name) (Join-Path $stage $name)
    }
    Copy-Item (Join-Path $Root 'assets\CREDITS.md') (Join-Path $stage 'CREDITS.md')
    Copy-Item $thirdParty (Join-Path $stage 'THIRD-PARTY.txt')
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $portable
    Remove-Item $stage -Recurse -Force
    Write-Host "Portable: $portable"

    $iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) was not found' }
    & $iscc "/DAppVersion=$Version" "/DSourceDir=$TargetDir\release" "/DThirdParty=$thirdParty" (Join-Path $Root 'installer\voice-button.iss')
    if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }
    Write-Host "Installer: $dist\voice-button-$Version-windows-x64-setup.exe"
}
