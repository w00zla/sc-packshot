# Shared by the PowerShell scripts: the pins in deps.toml and the verified Blender archive. Dot-source it after
# setting $root (the repo root).

# Native programs set $LASTEXITCODE but do not throw
function Assert-Ok([string]$what) {
  if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" }
}

# A value from deps.toml: `key = "value"` under `[section]`
function Get-Pin([string]$Section, [string]$Key) {
  $sec = ''
  foreach ($line in Get-Content (Join-Path $root 'deps.toml')) {
    if ($line -match '^\s*\[(.+)\]\s*$') { $sec = $Matches[1]; continue }
    if ($sec -eq $Section -and $line -match ('^\s*' + [regex]::Escape($Key) + '\s*=\s*"([^"]*)"')) { return $Matches[1] }
  }
  throw "deps.toml has no $Section.$Key"
}

# The pinned StarBreaker commit, shortened as the COMMIT files write it
function Get-PinnedCommit {
  return (Get-Pin 'starbreaker' 'commit').Substring(0, 8)
}

# The fork checkout from the pin (relative to the repo root)
function Get-ForkDir {
  return [System.IO.Path]::GetFullPath((Join-Path $root (Get-Pin 'starbreaker' 'repo')))
}

# The official Blender archive for a platform (windows-x64 | linux-x64), downloaded once into local/cache/ and checked
# against the SHA256 pinned in deps.toml; returns its path
function Get-BlenderArchive([string]$Platform) {
  $version = Get-Pin 'blender' 'version'
  $expected = (Get-Pin 'blender' "sha256-$Platform").ToUpperInvariant()
  $ext = if ($Platform -like 'windows*') { 'zip' } else { 'tar.xz' }
  $name = "blender-$version-$Platform.$ext"
  $cache = Join-Path $root 'local/cache'
  New-Item -ItemType Directory -Force -Path $cache | Out-Null
  $file = Join-Path $cache $name
  if (-not (Test-Path $file)) {
    $minor = ($version -split '\.')[0..1] -join '.'
    Write-Host "Downloading $name ..."
    $ProgressPreference = 'SilentlyContinue'  # else Invoke-WebRequest is extremely slow in 5.1
    Invoke-WebRequest -Uri "https://download.blender.org/release/Blender$minor/$name" -OutFile "$file.part" -UseBasicParsing
    Move-Item "$file.part" $file -Force
  }
  $actual = (Get-FileHash $file -Algorithm SHA256).Hash
  if ($actual -ne $expected) {
    Remove-Item -Force $file
    throw "SHA256 of $name is $actual, deps.toml pins ${expected}: download deleted"
  }
  Write-Host "SHA256 ok ($name)"
  return $file
}

# A file of the pinned StarBreaker release (windows-x64 | linux-x64 | addon), downloaded once into local/cache/ and
# checked against its SHA256 in deps.toml; returns its path
function Get-StarBreakerAsset([string]$Kind) {
  $release = Get-Pin 'starbreaker' 'release'
  $name = switch ($Kind) {
    'windows-x64' { "starbreaker-$release-windows-x86_64.zip" }
    'linux-x64' { "starbreaker-$release-linux-x86_64.tar.gz" }
    'addon' { "starbreaker_addon-$release.zip" }
    default { throw "Unknown StarBreaker release file: $Kind" }
  }
  $expected = (Get-Pin 'starbreaker' "sha256-$Kind").ToUpperInvariant()
  $cache = Join-Path $root 'local/cache'
  New-Item -ItemType Directory -Force -Path $cache | Out-Null
  $file = Join-Path $cache $name
  if (-not (Test-Path $file)) {
    Write-Host "Downloading $name ..."
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri "$(Get-Pin 'starbreaker' 'url')/releases/download/$release/$name" -OutFile "$file.part" -UseBasicParsing
    Move-Item "$file.part" $file -Force
  }
  $actual = (Get-FileHash $file -Algorithm SHA256).Hash
  if ($actual -ne $expected) {
    Remove-Item -Force $file
    throw "SHA256 of $name is $actual, deps.toml pins ${expected}: download deleted"
  }
  Write-Host "SHA256 ok ($name)"
  return $file
}

# The pinned release's binary for a platform (windows | linux) in local/starbreaker-ps/ with its .commit stamp, as
# starbreaker-build writes them; left alone when the stamp is the pin already. A binary of another commit (a build of
# the fork's HEAD) is not replaced: that throws.
function Install-StarBreaker([string]$Platform) {
  $out = Join-Path $root 'local/starbreaker-ps'
  $bin = if ($Platform -eq 'windows') { 'starbreaker.exe' } else { 'starbreaker' }
  $file = Join-Path $out $bin
  $pinned = Get-PinnedCommit
  if (Test-Path "$file.commit") {
    $stamp = ((Get-Content "$file.commit" -Raw).Trim() -split ' ')[0]
    if ($stamp -eq $pinned) { Write-Host "StarBreaker $stamp is there (local/starbreaker-ps/$bin)"; return }
    if (Test-Path $file) { throw "local/starbreaker-ps/$bin is $stamp, deps.toml pins ${pinned}: delete it to take the release" }
  }
  $asset = Get-StarBreakerAsset "$Platform-x64"
  New-Item -ItemType Directory -Force -Path $out | Out-Null
  tar -xf $asset -C $out; Assert-Ok "tar $asset"
  if (-not (Test-Path $file)) { throw "$asset holds no $bin" }
  $stamp = "$pinned starbreaker-ps $(Get-Pin 'starbreaker' 'release') (release)`n"
  [IO.File]::WriteAllText("$file.commit", $stamp, (New-Object Text.UTF8Encoding $false))
  if ($Platform -eq 'windows') { Copy-Item "$file.commit" (Join-Path $out 'COMMIT') -Force }
  Write-Host "StarBreaker $(Get-Pin 'starbreaker' 'release') ($pinned) in local/starbreaker-ps/$bin"
}

# The add-on of the pinned release, unpacked once into local/starbreaker-addon/<release>/; returns its folder
function Get-ReleaseAddon {
  $dir = Join-Path $root "local/starbreaker-addon/$(Get-Pin 'starbreaker' 'release')"
  $addon = Join-Path $dir 'starbreaker_addon'
  if (-not (Test-Path (Join-Path $addon '__init__.py'))) {
    $zip = Get-StarBreakerAsset 'addon'
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    tar -xf $zip -C $dir; Assert-Ok "tar $zip"
    if (-not (Test-Path (Join-Path $addon '__init__.py'))) { throw "$zip holds no starbreaker_addon/" }
  }
  return $addon
}
