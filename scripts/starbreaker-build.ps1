# Builds our StarBreaker fork at the commit pinned in deps.toml (or -Commit) into local/starbreaker-ps/, where packshot
# and the bundle scripts take it from:
#   starbreaker.exe          Windows (native build)
#   starbreaker              Linux, glibc build against glibc 2.31 (cargo-zigbuild), runs on every Linux from glibc 2.31 on
#   <binary>.commit          the commit each binary is built from (the bundles copy it as their COMMIT)
#   COMMIT                   the commit of this OS's binary (its first word is what packshot compares)
# Builds exactly that commit (git archive), whatever the fork's working tree holds.
# Run from the repo root (pwsh or Windows PowerShell 5.1):
#   scripts/starbreaker-build.ps1 [-Platform all|windows|linux] [-Commit <rev>] [-Fork <dir>]
# -Commit HEAD builds the fork's current commit, to try a change before pinning it.
# Needs: cargo-zigbuild and zig for the Linux target (winget install zig.zig; cargo install cargo-zigbuild;
# rustup target add x86_64-unknown-linux-gnu). scripts/starbreaker-build.sh does the same on Linux.
param(
  [string]$Fork = '',
  [string]$Commit = '',
  [ValidateSet('all', 'windows', 'linux')][string]$Platform = 'all'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
. (Join-Path $root 'scripts/lib/deps.ps1')
$Fork = if ($Fork) { [System.IO.Path]::GetFullPath((Join-Path $root $Fork)) } else { Get-ForkDir }
if (-not $Commit) { $Commit = Get-Pin 'starbreaker' 'commit' }
if (-not (Test-Path (Join-Path $Fork '.git'))) { throw "No StarBreaker checkout at $Fork (scripts/setup.ps1 clones it)" }
$out = Join-Path $root 'local/starbreaker-ps'
$work = Join-Path $root 'local/starbreaker-build'
$src = Join-Path $work 'src'
$target = Join-Path $work 'target'

# the commit, fetched if the checkout does not have it yet
git -C $Fork rev-parse --verify --quiet "$Commit^{commit}" | Out-Null
if ($LASTEXITCODE -ne 0) { git -C $Fork fetch --quiet origin; Assert-Ok 'git fetch' }
$full = (git -C $Fork rev-parse --verify "$Commit^{commit}").Trim(); Assert-Ok "git rev-parse $Commit"
$short = $full.Substring(0, 8)
$subject = (git -C $Fork log -1 --format=%s $full).Trim()
Write-Host "StarBreaker fork $Fork at ${short}: $subject"

# the committed tree, fresh; the target folder stays for incremental builds
if (Test-Path $src) { Remove-Item -Recurse -Force $src }
New-Item -ItemType Directory -Force -Path $src, $out | Out-Null
$tar = Join-Path $work 'src.tar'
git -C $Fork archive --format=tar -o $tar $full; Assert-Ok 'git archive'
tar -xf $tar -C $src; Assert-Ok 'tar'
Remove-Item -Force $tar

$stamp = "$short starbreaker-ps ($subject)`n"
$utf8 = New-Object Text.UTF8Encoding $false
Push-Location $src
try {
  if ($Platform -in 'all', 'windows') {
    Write-Host 'Building StarBreaker (Windows) ...'
    cargo build --release -p starbreaker --locked --target-dir $target
    Assert-Ok 'cargo build (Windows)'
    Copy-Item (Join-Path $target 'release/starbreaker.exe') (Join-Path $out 'starbreaker.exe') -Force
    [IO.File]::WriteAllText((Join-Path $out 'starbreaker.exe.commit'), $stamp, $utf8)
  }
  if ($Platform -in 'all', 'linux') {
    Write-Host 'Building StarBreaker (Linux, glibc 2.31) ...'
    cargo zigbuild --release -p starbreaker --locked --target x86_64-unknown-linux-gnu.2.31 --target-dir $target
    Assert-Ok 'cargo zigbuild (Linux)'
    Copy-Item (Join-Path $target 'x86_64-unknown-linux-gnu/release/starbreaker') (Join-Path $out 'starbreaker') -Force
    [IO.File]::WriteAllText((Join-Path $out 'starbreaker.commit'), $stamp, $utf8)
  }
} finally {
  Pop-Location
}

# COMMIT: this OS's binary (what packshot in this checkout runs)
$own = Join-Path $out 'starbreaker.exe.commit'
if (Test-Path $own) { Copy-Item $own (Join-Path $out 'COMMIT') -Force }
Write-Host "Done: $out ($short; $Platform)"
if ($full -ne (Get-Pin 'starbreaker' 'commit')) {
  Write-Host "Note: $short is not the commit deps.toml pins; the bundle scripts refuse it until deps.toml says so"
}
