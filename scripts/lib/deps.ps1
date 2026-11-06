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
