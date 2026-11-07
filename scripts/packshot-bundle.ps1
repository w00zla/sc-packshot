# Packs everything the Rust tool packshot needs on another machine, one folder per platform, built on
# Windows (scripts/packshot-bundle.sh builds the same on Linux):
#   <Out>/windows: packshot.exe, StarBreaker (our fork), the official Blender zip (SHA256 checked, left packed:
#                  setup.cmd unpacks it on the node), portable/ (config + add-on), render.py, targets.json, setup.cmd,
#                  run.cmd, README.txt, packshot.example.json (the Windows texts with CRLF)
#   <Out>/linux:   packshot (static musl binary), StarBreaker (the Linux build from local/starbreaker-ps/, made on
#                  Linux), the official Blender tarball (SHA256 checked), portable/ (config + add-on), render.py,
#                  targets.json, setup.sh and check.sh (unpack and check on the Linux node), run.sh, README.txt,
#                  packshot.example.json
# Run from the repo root (pwsh or Windows PowerShell 5.1):
#   scripts/packshot-bundle.ps1 [-Out local/packshot-bundle] [-Platform all|windows|linux]
#                               [-Targets local/packshot/targets.json]
# Blender and StarBreaker as deps.toml pins them: the Blender archives from local/cache/ (scripts/lib/deps.ps1), the
# StarBreaker binaries only when their <binary>.commit stamp is the pinned commit (a missing one comes from the pinned
# release, a build of another commit is refused: scripts/lib/deps.ps1 Install-StarBreaker),
# portable/ from the pinned local Blender (scripts/setup.ps1). The target list goes into both bundles when it exists,
# else they carry none.
param(
  [string]$Out = 'local/packshot-bundle',
  [ValidateSet('all', 'windows', 'linux')][string]$Platform = 'all',
  [string]$Targets = 'local/packshot/targets.json'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
. (Join-Path $root 'scripts/lib/deps.ps1')
$Out = [System.IO.Path]::GetFullPath((Join-Path $root $Out))
$tpl = Join-Path $root 'bundle'
$crate = $root
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$doWin = $Platform -in 'all', 'windows'
$doLin = $Platform -in 'all', 'linux'

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

# The pinned local Windows Blender: its portable/ (config and add-on) goes into both bundles
$blenderSrc = Get-Item (Join-Path $root "local/blender/blender-$(Get-Pin 'blender' 'version')-windows-x64")
if (-not (Test-Path (Join-Path $blenderSrc.FullName 'portable/config/userpref.blend'))) {
  throw "$($blenderSrc.FullName) has no set-up portable/ (scripts/setup.ps1)"
}
if ($doWin) { Install-StarBreaker 'windows' }
if ($doLin) { Install-StarBreaker 'linux' }

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

# The target list, if there is one
if (Test-Path $Targets) {
  $targets = (Resolve-Path $Targets).Path
  Write-Host "Target list: $targets"
} else {
  Write-Host "Note: no target list at $Targets, the bundles carry none"
  $targets = $null
}

# 2. Windows bundle
if ($doWin) {
  $win = Join-Path $Out 'windows'
  New-Item -ItemType Directory -Force -Path $win | Out-Null
  Copy-Item (Join-Path $crate 'target/release/packshot.exe') $win -Force

  New-Item -ItemType Directory -Force -Path (Join-Path $win 'starbreaker') | Out-Null
  Copy-Item local/starbreaker-ps/starbreaker.exe (Join-Path $win 'starbreaker') -Force
  Copy-Item local/starbreaker-ps/starbreaker.exe.commit (Join-Path $win 'starbreaker/COMMIT') -Force

  # Blender zip, packed (setup.cmd unpacks it); the unpacked blender/ of older bundles goes
  $zip = Get-BlenderArchive 'windows-x64'
  if (Test-Path (Join-Path $win 'blender')) { Remove-Item -Recurse -Force (Join-Path $win 'blender') }
  Get-ChildItem $win -Filter 'blender-*-windows-x64.zip' | Remove-Item -Force
  Copy-Item $zip $win -Force

  # portable/: config and add-on of the local Blender (the add-on is a junction: robocopy follows it with real files)
  Mirror (Join-Path $blenderSrc.FullName 'portable') (Join-Path $win 'portable')
  if (-not (Get-ChildItem (Join-Path $win 'portable/scripts/addons/starbreaker_addon') -Filter *.py -ErrorAction SilentlyContinue)) {
    throw 'portable/scripts/addons/starbreaker_addon holds no .py files'
  }

  New-Item -ItemType Directory -Force -Path (Join-Path $win 'cli') | Out-Null
  Copy-Item blender/render.py (Join-Path $win 'cli') -Force
  if ($targets) { Copy-Item $targets (Join-Path $win 'targets.json') -Force }

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
  Copy-Item local/starbreaker-ps/starbreaker (Join-Path $lin 'starbreaker') -Force
  Copy-Item local/starbreaker-ps/starbreaker.commit (Join-Path $lin 'starbreaker/COMMIT') -Force

  # Blender tarball, put into the bundle packed (unpacking on Windows would lose symlinks and exec bits: setup.sh
  # does that on the Linux node)
  $tar = Get-BlenderArchive 'linux-x64'
  Get-ChildItem $lin -Filter 'blender-*-linux-x64.tar.xz' | Remove-Item -Force
  Copy-Item $tar $lin -Force

  # portable/: config and add-on of the local Blender (the add-on is a junction: robocopy follows it with real files)
  Mirror (Join-Path $blenderSrc.FullName 'portable') (Join-Path $lin 'portable')
  if (-not (Get-ChildItem (Join-Path $lin 'portable/scripts/addons/starbreaker_addon') -Filter *.py -ErrorAction SilentlyContinue)) {
    throw 'portable/scripts/addons/starbreaker_addon holds no .py files'
  }

  New-Item -ItemType Directory -Force -Path (Join-Path $lin 'cli') | Out-Null
  Copy-Item blender/render.py (Join-Path $lin 'cli') -Force
  if ($targets) { Copy-Item $targets (Join-Path $lin 'targets.json') -Force }
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
