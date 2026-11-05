# Builds our StarBreaker fork (..\starbreaker-qm, github.com/w00zla/starbreaker-qm) at its HEAD for both platforms into
# local/starbreaker-qm/, where packshot and the bundle scripts take it from:
#   starbreaker.exe  Windows (native build)
#   starbreaker      Linux, glibc build against glibc 2.31 (cargo-zigbuild), runs on every Linux from glibc 2.31 on
#   COMMIT           the commit both are built from (its first word is what packshot compares)
# Builds exactly the committed HEAD (git archive), whatever the fork's working tree holds.
# Run from the repo root (pwsh or Windows PowerShell 5.1):
#   scripts/starbreaker-build.ps1 [-Platform all|windows|linux] [-Fork ..\starbreaker-qm]
# Needs: cargo-zigbuild and zig for the Linux target (winget install zig.zig; cargo install cargo-zigbuild).
# scripts/starbreaker-build.sh does the same on Linux.
param(
  [string]$Fork = '..\starbreaker-qm',
  [ValidateSet('all', 'windows', 'linux')][string]$Platform = 'all'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
$Fork = [System.IO.Path]::GetFullPath((Join-Path $root $Fork))
$out = Join-Path $root 'local/starbreaker-qm'
$work = Join-Path $root 'local/starbreaker-build'
$src = Join-Path $work 'src'
$target = Join-Path $work 'target'

# Native programs set $LASTEXITCODE but do not throw
function Assert-Ok([string]$what) {
  if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" }
}

$commit = (git -C $Fork rev-parse --short=8 HEAD).Trim(); Assert-Ok 'git rev-parse'
$branch = (git -C $Fork rev-parse --abbrev-ref HEAD).Trim()
$subject = (git -C $Fork log -1 --format=%s).Trim()
Write-Host "StarBreaker fork $Fork at $commit ($branch): $subject"

# the committed tree, fresh; the target folder stays for incremental builds
if (Test-Path $src) { Remove-Item -Recurse -Force $src }
New-Item -ItemType Directory -Force -Path $src, $out | Out-Null
$tar = Join-Path $work 'src.tar'
git -C $Fork archive --format=tar -o $tar HEAD; Assert-Ok 'git archive'
tar -xf $tar -C $src; Assert-Ok 'tar'
Remove-Item -Force $tar

Push-Location $src
try {
  if ($Platform -in 'all', 'windows') {
    Write-Host 'Building StarBreaker (Windows) ...'
    cargo build --release -p starbreaker --locked --target-dir $target
    Assert-Ok 'cargo build (Windows)'
    Copy-Item (Join-Path $target 'release/starbreaker.exe') (Join-Path $out 'starbreaker.exe') -Force
  }
  if ($Platform -in 'all', 'linux') {
    Write-Host 'Building StarBreaker (Linux, glibc 2.31) ...'
    cargo zigbuild --release -p starbreaker --locked --target x86_64-unknown-linux-gnu.2.31 --target-dir $target
    Assert-Ok 'cargo zigbuild (Linux)'
    Copy-Item (Join-Path $target 'x86_64-unknown-linux-gnu/release/starbreaker') (Join-Path $out 'starbreaker') -Force
  }
} finally {
  Pop-Location
}

# COMMIT: a hand-written description after the hash stays while the commit is the same
$commitFile = Join-Path $out 'COMMIT'
$old = if (Test-Path $commitFile) { (Get-Content $commitFile -Raw).Trim() } else { '' }
if (($old -split ' ')[0] -ne $commit) {
  [IO.File]::WriteAllText($commitFile, "$commit starbreaker-qm $branch ($subject)`n", (New-Object Text.UTF8Encoding $false))
}
Write-Host "Done: $out (COMMIT $commit)"
if ($Platform -ne 'all') {
  Write-Host "Note: only $Platform was built; the other binary in $out may be from another commit than COMMIT says"
}
