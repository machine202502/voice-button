# Build or test the local checkout without tripping the Windows path limit.
#
# whisper.cpp's Vulkan shader generator needs the build directory path to stay
# under 259 characters, and the long form of this checkout is five characters too
# long. The 8.3 short name of the same directory fits, and passing it through
# CARGO_TARGET_DIR works from any working directory.
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CargoArgs)

$ErrorActionPreference = 'Continue'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
if (-not $CargoArgs) { $CargoArgs = @('build', '--release', '--bins') }

if (-not $env:CARGO_TARGET_DIR) {
    $short = (& cmd /c "for %I in (`"$Root`") do @echo %~sI") 2>$null
    if (-not $short) { $short = $Root.Path }
    $env:CARGO_TARGET_DIR = Join-Path $short.Trim() 'tmp'
}
Write-Host "build directory: $env:CARGO_TARGET_DIR"
Set-Location $Root
& cargo @CargoArgs
exit $LASTEXITCODE
