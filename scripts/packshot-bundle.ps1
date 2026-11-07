# Packs everything the Rust tool packshot needs on another machine, one folder per platform, built on
# Windows (scripts/packshot-bundle.sh builds the same on Linux):
#   <Out>/windows: packshot.exe, starbreaker/ (our fork, with COMMIT), addon/starbreaker_addon, cli/render.py,
#                  profiles/, blender.pin, targets/targets.json, setup.cmd, check.cmd, run.cmd, README.txt,
#                  packshot.example.json (CRLF)
#   <Out>/linux:   packshot (static musl binary), starbreaker/ (the fork's Linux binary, with COMMIT),
#                  addon/starbreaker_addon, cli/render.py, profiles/, blender.pin, targets/targets.json, setup.sh,
#                  check.sh, run.sh, README.txt, packshot.example.json
# No Blender in the bundles: setup.cmd / setup.sh take the official archive named in blender.pin (next to them, else
# from blender.org), check its SHA256 and put portable/ with the add-on into it.
# Run from the repo root (pwsh or Windows PowerShell 5.1):
#   scripts/packshot-bundle.ps1 [-Out local/packshot-bundle] [-Platform all|windows|linux]
#                               [-Targets local/packshot/targets/targets.json]
# Blender and StarBreaker as deps.toml pins them: the StarBreaker binaries only when their <binary>.commit stamp is the
# pinned commit (a missing one comes from the pinned release, a build of another commit is refused:
# scripts/lib/deps.ps1 Install-StarBreaker), the add-on from the pinned release. The target list goes into both
# bundles as targets/targets.json when it exists, else they carry none.
param(
  [string]$Out = 'local/packshot-bundle',
  [ValidateSet('all', 'windows', 'linux')][string]$Platform = 'all',
  [string]$Targets = 'local/packshot/targets/targets.json'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
. (Join-Path $root 'scripts/lib/deps.ps1')
$Out = [System.IO.Path]::GetFullPath((Join-Path $root $Out))
$tpl = Join-Path $root 'templates/bundle'
$crate = $root
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$doWin = $Platform -in 'all', 'windows'
$doLin = $Platform -in 'all', 'linux'

# The pinned release's add-on as real files and the Blender pin for one platform (windows-x64 | linux-x64); what older
# bundles carried instead (the Blender archive, portable/) goes
function Add-AddonAndPin([string]$dir, [string]$platform, [switch]$Crlf) {
  $addon = Get-ReleaseAddon
  foreach ($old in 'addon', 'portable') {
    if (Test-Path (Join-Path $dir $old)) { Remove-Item -Recurse -Force (Join-Path $dir $old) }
  }
  Get-ChildItem $dir -File -Filter 'blender-*' | Remove-Item -Force
  New-Item -ItemType Directory -Force -Path (Join-Path $dir 'addon') | Out-Null
  Copy-Item $addon (Join-Path $dir 'addon/starbreaker_addon') -Recurse
  Get-ChildItem (Join-Path $dir 'addon') -Recurse -Directory -Filter __pycache__ | Remove-Item -Recurse -Force
  $nl = if ($Crlf) { "`r`n" } else { "`n" }
  $pin = "version=$(Get-Pin 'blender' 'version')${nl}sha256=$(Get-Pin 'blender' "sha256-$platform")$nl"
  [IO.File]::WriteAllText((Join-Path $dir 'blender.pin'), $pin, (New-Object Text.UTF8Encoding $false))
}

# The bundled profiles beside cli/ (render.py finds game-tile there), each replaced whole; own ones in profiles/ stay
function Add-Profiles([string]$dir) {
  New-Item -ItemType Directory -Force -Path (Join-Path $dir 'profiles') | Out-Null
  foreach ($p in Get-ChildItem (Join-Path $root 'profiles') -Directory) {
    $dst = Join-Path $dir "profiles/$($p.Name)"
    if (Test-Path $dst) { Remove-Item -Recurse -Force $dst }
    Copy-Item $p.FullName $dst -Recurse
    Get-ChildItem $dst -Recurse -Directory -Filter __pycache__ | Remove-Item -Recurse -Force
  }
}

# Copy a template with LF line endings (the .sh need LF, however the template was checked out), -Crlf for the
# Windows bundle's texts
function Copy-Template([string]$src, [string]$dst, [switch]$Crlf) {
  $text = [IO.File]::ReadAllText($src).Replace("`r`n", "`n")
  if ($Crlf) { $text = $text.Replace("`n", "`r`n") }
  $enc = if ($dst -like '*.txt') { New-Object Text.UTF8Encoding $true } else { New-Object Text.UTF8Encoding $false }
  [IO.File]::WriteAllText($dst, $text, $enc)
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

  Add-AddonAndPin $win 'windows-x64' -Crlf

  New-Item -ItemType Directory -Force -Path (Join-Path $win 'cli') | Out-Null
  Copy-Item blender/render.py (Join-Path $win 'cli') -Force
  Add-Profiles $win
  if ($targets) {
    New-Item -ItemType Directory -Force -Path (Join-Path $win 'targets') | Out-Null
    Copy-Item $targets (Join-Path $win 'targets/targets.json') -Force
  }

  foreach ($f in 'setup.cmd', 'check.cmd', 'run.cmd', 'README.txt') { Copy-Template (Join-Path $tpl "windows/$f") (Join-Path $win $f) -Crlf }
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

  Add-AddonAndPin $lin 'linux-x64'

  New-Item -ItemType Directory -Force -Path (Join-Path $lin 'cli') | Out-Null
  Copy-Item blender/render.py (Join-Path $lin 'cli') -Force
  Add-Profiles $lin
  if ($targets) {
    New-Item -ItemType Directory -Force -Path (Join-Path $lin 'targets') | Out-Null
    Copy-Item $targets (Join-Path $lin 'targets/targets.json') -Force
  }
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
exit 0
