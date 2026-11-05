# Packs everything the Rust tool packshot (tools/packshot) needs on another machine, one folder per platform, built on
# Windows (scripts/packshot-bundle.sh builds the same on Linux):
#   <Out>/windows: packshot.exe, StarBreaker (our fork), the official Blender zip (SHA256 checked, left packed:
#                  setup.cmd unpacks it on the node), portable/ (config + add-on), render.py, targets.json, setup.cmd,
#                  run.cmd, README.txt, packshot.example.json (the Windows texts with CRLF)
#   <Out>/linux:   packshot (static musl binary), StarBreaker (the Linux build from local/starbreaker-qm/, made on
#                  Linux), the official Blender tarball (SHA256 checked), portable/ (config + add-on), render.py,
#                  targets.json, setup.sh and check.sh (unpack and check on the Linux node), run.sh, README.txt,
#                  packshot.example.json
# Run from the repo root (pwsh or Windows PowerShell 5.1):
#   scripts/packshot-bundle.ps1 [-Out local/packshot-bundle] [-BlenderVersion 5.2.3] [-Platform all|windows|linux]
param(
  [string]$Out = 'local/packshot-bundle',
  [string]$BlenderVersion = '5.2.3',
  [ValidateSet('all', 'windows', 'linux')][string]$Platform = 'all'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
$Out = [System.IO.Path]::GetFullPath((Join-Path $root $Out))
$tpl = Join-Path $root 'tools/packshot/bundle'
$crate = Join-Path $root 'tools/packshot'
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$doWin = $Platform -in 'all', 'windows'
$doLin = $Platform -in 'all', 'linux'

# Native programs set $LASTEXITCODE but do not throw
function Assert-Ok([string]$what) {
  if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" }
}

# Mirror a folder: robocopy exit codes < 8 are success (1 = copied, 0 = unchanged)
function Mirror([string]$src, [string]$dst) {
  robocopy $src $dst /MIR /XD __pycache__ /NFL /NDL /NJH /NJS /NP | Out-Null
  if ($LASTEXITCODE -ge 8) { throw "robocopy $src failed ($LASTEXITCODE)" }
}

# Copy a template with LF line endings (the .sh need LF, however the template was checked out), -Crlf for the
# Windows bundle's texts
function Copy-Template([string]$src, [string]$dst, [switch]$Crlf) {
  $text = [IO.File]::ReadAllText($src).Replace("`r`n", "`n")
  if ($Crlf) { $text = $text.Replace("`n", "`r`n") }
  $enc = if ($dst -like '*.txt') { New-Object Text.UTF8Encoding $true } else { New-Object Text.UTF8Encoding $false }
  [IO.File]::WriteAllText($dst, $text, $enc)
}

# Blender archive (blender-<ver>-<platform>): downloaded once into cache/, checked against the published SHA256;
# returns its path
function Get-Blender([string]$name) {
  $minor = ($BlenderVersion -split '\.')[0..1] -join '.'
  $base = "https://download.blender.org/release/Blender$minor"
  $file = Join-Path $cache $name
  if (-not (Test-Path $file)) {
    Write-Host "Downloading $name ..."
    $ProgressPreference = 'SilentlyContinue'  # else Invoke-WebRequest is extremely slow in 5.1
    Invoke-WebRequest -Uri "$base/$name" -OutFile "$file.part" -UseBasicParsing
    Move-Item "$file.part" $file -Force
  }
  $sums = (Invoke-WebRequest -Uri "$base/blender-$BlenderVersion.sha256" -UseBasicParsing).Content
  $expected = ($sums -split "`n" | Where-Object { $_ -match [regex]::Escape($name) } | Select-Object -First 1) -replace '\s.*$', ''
  if (-not $expected) { throw "No SHA256 for $name in $base/blender-$BlenderVersion.sha256" }
  $actual = (Get-FileHash $file -Algorithm SHA256).Hash
  if ($actual -ne $expected.Trim().ToUpperInvariant()) {
    Remove-Item -Force $file
    throw "SHA256 mismatch ($actual, expected $expected): download deleted"
  }
  Write-Host "SHA256 ok ($name)"
  return $file
}

# Folder of the local Windows Blender (with blender.exe): its portable/ goes into both bundles
$blenderSrc = Get-ChildItem local/blender -Directory | Where-Object { Test-Path (Join-Path $_.FullName 'blender.exe') } | Select-Object -First 1
if (-not $blenderSrc) { throw 'No Blender under local/blender/*/blender.exe' }
if ($doLin -and -not (Test-Path local/starbreaker-qm/starbreaker)) {
  throw 'No local/starbreaker-qm/starbreaker: the Linux build of the fork is made on Linux (cargo build --release -p starbreaker)'
}

# 1. Build
Push-Location $crate
try {
  if ($doWin) {
    Write-Host 'Building packshot (Windows) ...'
    cargo build --release
    Assert-Ok 'cargo build (Windows)'
  }
  if ($doLin) {
    Write-Host 'Building packshot (Linux, musl) ...'
    cargo build --release --target x86_64-unknown-linux-musl
    Assert-Ok 'cargo build (Linux)'
  }
} finally {
  Pop-Location
}

# Target list from the DB once for both (needs DATABASE_URL from .env)
$cache = Join-Path $Out 'cache'
New-Item -ItemType Directory -Force -Path $cache | Out-Null
$targets = Join-Path $cache 'targets.json'
Write-Host 'Target list from the DB ...'
pnpm packshot:targets --out $targets
Assert-Ok 'writing targets.json'

# 2. Windows bundle
if ($doWin) {
  $win = Join-Path $Out 'windows'
  New-Item -ItemType Directory -Force -Path $win | Out-Null
  Copy-Item (Join-Path $crate 'target/release/packshot.exe') $win -Force

  New-Item -ItemType Directory -Force -Path (Join-Path $win 'starbreaker') | Out-Null
  Copy-Item local/starbreaker-qm/starbreaker.exe, local/starbreaker-qm/COMMIT (Join-Path $win 'starbreaker') -Force

  # Blender zip, packed (setup.cmd unpacks it); the unpacked blender/ of older bundles goes
  $zip = Get-Blender "blender-$BlenderVersion-windows-x64.zip"
  if (Test-Path (Join-Path $win 'blender')) { Remove-Item -Recurse -Force (Join-Path $win 'blender') }
  Get-ChildItem $win -Filter 'blender-*-windows-x64.zip' | Remove-Item -Force
  Copy-Item $zip $win -Force

  # portable/: config and add-on of the local Blender (the add-on is a junction: robocopy follows it with real files)
  Mirror (Join-Path $blenderSrc.FullName 'portable') (Join-Path $win 'portable')
  if (-not (Get-ChildItem (Join-Path $win 'portable/scripts/addons/starbreaker_addon') -Filter *.py -ErrorAction SilentlyContinue)) {
    throw 'portable/scripts/addons/starbreaker_addon holds no .py files'
  }

  New-Item -ItemType Directory -Force -Path (Join-Path $win 'cli') | Out-Null
  Copy-Item src/icons/render.py (Join-Path $win 'cli') -Force
  Copy-Item $targets $win -Force

  foreach ($f in 'setup.cmd', 'run.cmd', 'README.txt') { Copy-Template (Join-Path $tpl "windows/$f") (Join-Path $win $f) -Crlf }
  Copy-Template (Join-Path $tpl 'packshot.example.json') (Join-Path $win 'packshot.example.json') -Crlf
  Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $win 'LIESMICH.txt')
}

# 3. Linux bundle
if ($doLin) {
  $lin = Join-Path $Out 'linux'
  New-Item -ItemType Directory -Force -Path $lin | Out-Null
  Copy-Item (Join-Path $crate 'target/x86_64-unknown-linux-musl/release/packshot') $lin -Force

  # StarBreaker for Linux: linked against glibc, the node needs at least the one it was built with
  # (scripts/packshot-bundle.sh on Linux prints it)
  New-Item -ItemType Directory -Force -Path (Join-Path $lin 'starbreaker') | Out-Null
  Copy-Item local/starbreaker-qm/starbreaker, local/starbreaker-qm/COMMIT (Join-Path $lin 'starbreaker') -Force

  # Blender tarball, put into the bundle packed (unpacking on Windows would lose symlinks and exec bits: setup.sh
  # does that on the Linux node)
  $tar = Get-Blender "blender-$BlenderVersion-linux-x64.tar.xz"
  Get-ChildItem $lin -Filter 'blender-*-linux-x64.tar.xz' | Remove-Item -Force
  Copy-Item $tar $lin -Force

  # portable/: config and add-on of the local Blender (the add-on is a junction: robocopy follows it with real files)
  Mirror (Join-Path $blenderSrc.FullName 'portable') (Join-Path $lin 'portable')
  if (-not (Get-ChildItem (Join-Path $lin 'portable/scripts/addons/starbreaker_addon') -Filter *.py -ErrorAction SilentlyContinue)) {
    throw 'portable/scripts/addons/starbreaker_addon holds no .py files'
  }

  New-Item -ItemType Directory -Force -Path (Join-Path $lin 'cli') | Out-Null
  Copy-Item src/icons/render.py (Join-Path $lin 'cli') -Force
  Copy-Item $targets $lin -Force
  foreach ($f in 'setup.sh', 'check.sh', 'run.sh', 'README.txt') { Copy-Template (Join-Path $tpl "linux/$f") (Join-Path $lin $f) }
  Copy-Template (Join-Path $tpl 'packshot.example.json') (Join-Path $lin 'packshot.example.json')
  Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $lin 'LIESMICH.txt'), (Join-Path $lin 'run-node.sh')
}

# 4. Summary
foreach ($p in @(if ($doWin) { 'windows' }) + @(if ($doLin) { 'linux' })) {
  $files = Get-ChildItem (Join-Path $Out $p) -Recurse -File
  $mb = [math]::Round(($files | Measure-Object Length -Sum).Sum / 1MB)
  Write-Host "Bundle $p`: $(Join-Path $Out $p) ($mb MB, $($files.Count) files)"
}
exit 0  # robocopy's exit code 1 (copied) must not count as a failure
