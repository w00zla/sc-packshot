# Sets up a checkout for development on Windows, exactly as deps.toml pins it (run again after changing a pin;
# what is already right is left as it is):
#   1. the StarBreaker add-on of the pinned release (local/starbreaker-addon/<release>/, SHA256 checked); with
#      -FromSource the fork next to the repo (deps.toml starbreaker.repo) instead, cloned if it is missing
#   2. the pinned Blender in local/blender/blender-<version>-windows-x64/, from the official archive (SHA256 checked)
#   3. its portable/ folder (Blender keeps its config there, nothing in the user profile) with the add-on linked in (a
#      junction; with -FromSource into the fork, so changes there apply at once) and enabled in
#      portable/config/userpref.blend
#   4. StarBreaker (Windows binary) into local/starbreaker-ps/: from the pinned release, with -FromSource built at the
#      pinned commit
# Run from the repo root (pwsh or Windows PowerShell 5.1):
#   scripts/setup.ps1 [-FromSource] [-SkipStarBreaker]
# The bundles (scripts/packshot-bundle.ps1) take portable/ from here, so set it up before building them.
param([switch]$FromSource, [switch]$SkipStarBreaker)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
. (Join-Path $root 'scripts/lib/deps.ps1')

# 1. the add-on: the release's, or the fork's
if ($FromSource) {
  $fork = Get-ForkDir
  if (-not (Test-Path (Join-Path $fork '.git'))) {
    Write-Host "Cloning the StarBreaker fork into $fork ..."
    git clone --quiet (Get-Pin 'starbreaker' 'url') $fork; Assert-Ok 'git clone'
  }
  $addonSrc = Join-Path $fork 'blender_addon/starbreaker_addon'
} else {
  $addonSrc = Get-ReleaseAddon
}
if (-not (Test-Path (Join-Path $addonSrc '__init__.py'))) { throw "No add-on in $addonSrc" }

# 2. Blender
$version = Get-Pin 'blender' 'version'
$dir = Join-Path $root "local/blender/blender-$version-windows-x64"
$exe = Join-Path $dir 'blender.exe'
if (Test-Path $exe) {
  Write-Host "Blender $version is there ($dir)"
} else {
  $zip = Get-BlenderArchive 'windows-x64'
  New-Item -ItemType Directory -Force -Path (Join-Path $root 'local/blender') | Out-Null
  Write-Host "Unpacking Blender $version ..."
  tar -xf $zip -C (Join-Path $root 'local/blender'); Assert-Ok 'tar'
  if (-not (Test-Path $exe)) { throw "The archive did not unpack to $dir" }
}

# 3. portable/ with the add-on as a junction
$addons = Join-Path $dir 'portable/scripts/addons'
New-Item -ItemType Directory -Force -Path $addons, (Join-Path $dir 'portable/config') | Out-Null
$link = Join-Path $addons 'starbreaker_addon'
$item = Get-Item $link -Force -ErrorAction SilentlyContinue
if ($item -and -not $item.LinkType) {
  throw "$link is a real folder, not a link: move it away, then run this again"
}
if ($item -and ([System.IO.Path]::GetFullPath([string]$item.Target) -ne [System.IO.Path]::GetFullPath($addonSrc))) {
  cmd /c rmdir "$link" | Out-Null  # removes the junction only, never its target
  $item = $null
}
if (-not $item) {
  New-Item -ItemType Junction -Path $link -Target $addonSrc | Out-Null
  Write-Host "Add-on linked: $link -> $addonSrc"
}
$check = 'import bpy, addon_utils; addon_utils.enable("starbreaker_addon", default_set=True, persistent=True); bpy.ops.wm.save_userpref(); print("ADDON", "starbreaker_addon" in bpy.context.preferences.addons)'
$res = & $exe -b --python-expr $check 2>&1 | Select-String -SimpleMatch 'ADDON '
if ("$res" -notmatch 'ADDON True') { throw "Blender did not enable the add-on: $res" }
Write-Host 'Add-on enabled in portable/config/userpref.blend'

# 4. StarBreaker at the pin: the release's binary, or built from the fork
if (-not $SkipStarBreaker) {
  if ($FromSource) {
    $stampFile = Join-Path $root 'local/starbreaker-ps/starbreaker.exe.commit'
    $stamp = if (Test-Path $stampFile) { ((Get-Content $stampFile -Raw).Trim() -split ' ')[0] } else { '' }
    if ($stamp -eq (Get-PinnedCommit)) {
      Write-Host "StarBreaker $stamp is there (local/starbreaker-ps)"
    } else {
      & (Join-Path $root 'scripts/starbreaker-build.ps1') -Platform windows
      if (-not $?) { throw 'StarBreaker build failed' }
    }
  } else {
    Install-StarBreaker 'windows'
  }
}
Write-Host 'Ready: cargo run --release -- --render --export'
