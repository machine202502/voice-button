# Build the new Svelte phone page and stage it as static files.
#
# The phone page is built from `web/` straight into `src/web`, the directory the
# daemon serves. Nothing of the page is committed: `src/web` is in .gitignore.
[CmdletBinding()]
param(
    [string]$Out = 'src/web',
    [string]$Source = 'web'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot

function Resolve-ProjectPath {
    param([string]$Path)
    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $Path))
}

$webDir = Resolve-ProjectPath $Source
$outDir = Resolve-ProjectPath $Out

$repoFull = [System.IO.Path]::GetFullPath($repoRoot).TrimEnd('\', '/')
$webFull = $webDir.TrimEnd('\', '/')
$outFull = $outDir.TrimEnd('\', '/')

if ($outFull -ieq $webFull -or $outFull.StartsWith($webFull + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to write to ${outDir}: it is inside the web source directory."
}
if (-not $outFull.StartsWith($repoFull + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to write outside the repository: $outDir"
}

if (-not (Test-Path -LiteralPath $webDir -PathType Container)) {
    throw "Source directory not found: $webDir"
}
$packageJson = Join-Path $webDir 'package.json'
if (-not (Test-Path -LiteralPath $packageJson -PathType Leaf)) {
    throw "package.json not found in $webDir"
}

# Prefer the DSH runtime that this repository is built with, then fall back
# to whatever the machine has on PATH.
$nodeExe = $null
$pnpmMjs = $null
$pnpmExe = $null

if ($env:DSH_HOME) {
    $dshDeps = Join-Path $env:DSH_HOME 'dsh-runtimes\dsh-primary-runtime\dependencies'
    $dshNode = Join-Path $dshDeps 'node\bin\node.exe'
    $dshPnpm = Join-Path $dshDeps 'pnpm\bin\pnpm.mjs'
    if ((Test-Path -LiteralPath $dshNode -PathType Leaf) -and (Test-Path -LiteralPath $dshPnpm -PathType Leaf)) {
        $nodeExe = $dshNode
        $pnpmMjs = $dshPnpm
    }
}

if (-not $nodeExe) {
    $nodeCommand = Get-Command node -ErrorAction SilentlyContinue
    if ($nodeCommand) { $nodeExe = $nodeCommand.Source }
}
if (-not $pnpmMjs) {
    $pnpmCommand = Get-Command pnpm -ErrorAction SilentlyContinue
    if ($pnpmCommand) { $pnpmExe = $pnpmCommand.Source }
}

if (-not $nodeExe) {
    throw 'node was not found. Install Node.js or add node.exe to PATH, then run this script again.'
}
if (-not $pnpmMjs -and -not $pnpmExe) {
    throw 'pnpm was not found. Install pnpm or add it to PATH, then run this script again.'
}

function Invoke-Pnpm {
    param([string[]]$PnpmArgs)
    if ($pnpmMjs) {
        & $nodeExe $pnpmMjs @PnpmArgs
    }
    else {
        & $pnpmExe @PnpmArgs
    }
}

Write-Host "==> Building from $webDir"
Write-Host "    node: $nodeExe"
if ($pnpmMjs) { Write-Host "    pnpm: $pnpmMjs" } else { Write-Host "    pnpm: $pnpmExe" }

$nodeModules = Join-Path $webDir 'node_modules'
if (-not (Test-Path -LiteralPath $nodeModules -PathType Container)) {
    Write-Host ''
    Write-Host '==> Installing dependencies (node_modules is missing)'
    Push-Location $webDir
    try {
        Invoke-Pnpm @('install')
        if ($LASTEXITCODE -ne 0) {
            throw "pnpm install failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }
    if (-not (Test-Path -LiteralPath $nodeModules -PathType Container)) {
        throw "pnpm install finished but $nodeModules is still missing."
    }
}

Push-Location $webDir
try {
    Write-Host ''
    Write-Host '==> Type check (svelte-check)'
    Invoke-Pnpm @('run', 'check')
    if ($LASTEXITCODE -ne 0) {
        throw "svelte-check failed with exit code $LASTEXITCODE. The build was not run; fix the TypeScript diagnostics above."
    }

    Write-Host ''
    Write-Host '==> Build (vite build)'
    Invoke-Pnpm @('run', 'build')
    if ($LASTEXITCODE -ne 0) {
        throw "vite build failed with exit code $LASTEXITCODE."
    }
}
finally {
    Pop-Location
}

$distDir = Join-Path $webDir 'dist'
$distIndex = Join-Path $distDir 'index.html'
if (-not (Test-Path -LiteralPath $distIndex -PathType Leaf)) {
    throw "The build did not produce $distIndex."
}

Write-Host ''
Write-Host "==> Staging into $outDir"
if (Test-Path -LiteralPath $outDir) {
    Remove-Item -LiteralPath $outDir -Recurse -Force
}
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
Copy-Item -Path (Join-Path $distDir '*') -Destination $outDir -Recurse -Force

$installPage = Join-Path $outDir 'install.html'
if (-not (Test-Path -LiteralPath $installPage -PathType Leaf)) {
    throw "install.html was not produced. Keep it in web/public so Vite copies it into the build."
}

# Keep the content the current page already ships.  Read it from disk; do not
# duplicate it here.  No JavaScript or CSS is copied from src/web.
# Translations, icons, manifests and version.json live in `web/public` and Vite
# copies that directory into the build, so nothing is staged from the old page.

# Build the service worker's precache list from the files the build actually
# emitted.  The entry names come from the built index.html; any extra chunks
# in dist/assets are included as well.
$indexText = Get-Content -LiteralPath (Join-Path $outDir 'index.html') -Raw
$precache = [System.Collections.Generic.List[string]]::new()
$precache.Add('')
$precache.Add('index.html')

foreach ($match in [regex]::Matches($indexText, '(?:src|href)="([^"]+)"')) {
    $raw = $match.Groups[1].Value
    if ($raw -match '^(?:[a-zA-Z][a-zA-Z0-9+.-]*:|#)') { continue }
    $clean = ($raw -split '[?#]')[0]
    $clean = $clean.TrimStart('/')
    $clean = $clean -replace '^\./', ''
    if ($clean -and -not $precache.Contains($clean)) {
        $precache.Add($clean)
    }
}

$assetsRoot = Join-Path $outDir 'assets'
if (Test-Path -LiteralPath $assetsRoot -PathType Container) {
    Get-ChildItem -LiteralPath $assetsRoot -Recurse -File | ForEach-Object {
        $relative = $_.FullName.Substring($outDir.Length + 1).Replace('\', '/')
        if (-not $precache.Contains($relative)) {
            $precache.Add($relative)
        }
    }
}

# The data the page needs to work with no network at all: the translations, the
# manifests, the icons and the version.  A page whose catalogs are missing falls
# back to raw keys, so offline it would show "create" instead of "Создать".
foreach ($folder in @('lang', 'icons')) {
    $dir = Join-Path $outDir $folder
    if (-not (Test-Path -LiteralPath $dir -PathType Container)) { continue }
    Get-ChildItem -LiteralPath $dir -File | ForEach-Object {
        $relative = $_.FullName.Substring($outDir.Length + 1).Replace('\', '/')
        if (-not $precache.Contains($relative)) {
            $precache.Add($relative)
        }
    }
}
foreach ($name in @('version.json', 'favicon.svg', 'install.html')) {
    if ((Test-Path -LiteralPath (Join-Path $outDir $name) -PathType Leaf) -and -not $precache.Contains($name)) {
        $precache.Add($name)
    }
}
Get-ChildItem -LiteralPath $outDir -Filter 'manifest.*.webmanifest' -File | ForEach-Object {
    if (-not $precache.Contains($_.Name)) {
        $precache.Add($_.Name)
    }
}

$missing = @()
foreach ($relative in $precache) {
    if (-not $relative) { continue }
    $candidate = Join-Path $outDir ($relative -replace '/', '\')
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        $missing += $relative
    }
}
if ($missing.Count -gt 0) {
    throw "index.html references files that were not written: $($missing -join ', ')"
}

$assetLines = ($precache | ForEach-Object { '  "' + ($_ -replace '\\', '\\') + '"' }) -join ",`n"
$indexHash = (Get-FileHash -LiteralPath (Join-Path $outDir 'index.html') -Algorithm SHA256).Hash.Substring(0, 8).ToLowerInvariant()
$cacheName = "voice-button-web-$indexHash"

$swTemplate = @'
/* Generated by scripts/web.ps1.  Caches the assets named in index.html and
   falls back to the network; there is no code for an older page here. */
const CACHE = '__CACHE__';
const PRECACHE = [
__ASSETS__
];
const SCOPE = self.registration.scope;

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(PRECACHE.map((path) => new URL(path, SCOPE).href)))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  event.respondWith(
    caches.match(request).then((cached) => {
      if (cached) return cached;
      return fetch(request).catch(() => {
        if (request.mode === 'navigate') {
          return caches.match(new URL('index.html', SCOPE).href);
        }
        return Response.error();
      });
    })
  );
});
'@

$sw = $swTemplate.Replace('__CACHE__', $cacheName).Replace('__ASSETS__', $assetLines)
$swPath = Join-Path $outDir 'sw.js'
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($swPath, $sw, $utf8NoBom)
if (-not (Test-Path -LiteralPath $swPath -PathType Leaf)) {
    throw "The service worker was not written: $swPath"
}

$builtFiles = @(Get-ChildItem -LiteralPath $outDir -Recurse -File | Sort-Object FullName)
Write-Host ''
Write-Host "==> Done: $outDir"
Write-Host "    $($builtFiles.Count) files"
foreach ($file in $builtFiles) {
    $relative = $file.FullName.Substring($outDir.Length + 1).Replace('\', '/')
    Write-Host "    $relative"
}

# pnpm writes its banner to stderr, which Windows PowerShell turns into an
# error record; the script itself must still report success to its caller.
exit 0
