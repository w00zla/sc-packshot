#!/usr/bin/env bash
# Shared by the shell scripts: the pins in deps.toml and the verified Blender archive. Source it from the repo root.
# shellcheck shell=bash

# A value from deps.toml: `key = "value"` under `[section]`
pin() {
  local value
  value=$(awk -v want="[$1]" -v key="$2" '
    /^[[:space:]]*\[/ { section = $0; gsub(/[[:space:]]/, "", section); next }
    section == want {
      line = $0
      if (match(line, "^[[:space:]]*" key "[[:space:]]*=[[:space:]]*\"")) {
        line = substr(line, RLENGTH + 1)
        sub(/".*$/, "", line)
        print line
        exit
      }
    }' deps.toml)
  if [[ -z $value ]]; then
    echo "ERROR: deps.toml has no $1.$2" >&2
    return 1
  fi
  printf '%s\n' "$value"
}

# The pinned StarBreaker commit, shortened as the COMMIT files write it
pinned_commit() {
  local c
  c=$(pin starbreaker commit) || return 1
  printf '%s\n' "${c:0:8}"
}

# The fork checkout from the pin
fork_dir() {
  pin starbreaker repo
}

# The official Blender archive for a platform (windows-x64 | linux-x64), downloaded once into local/cache/ and
# checked against the SHA256 pinned in deps.toml; prints its path
blender_archive() {
  local platform=$1 version expected ext name file minor actual
  version=$(pin blender version) || return 1
  expected=$(pin blender "sha256-$platform") || return 1
  if [[ $platform == windows* ]]; then ext=zip; else ext=tar.xz; fi
  name=blender-$version-$platform.$ext
  mkdir -p local/cache
  file=local/cache/$name
  if [[ ! -f $file ]]; then
    minor=$(cut -d. -f1-2 <<<"$version")
    echo "Downloading $name ..." >&2
    curl -fL --retry 3 -o "$file.part" "https://download.blender.org/release/Blender$minor/$name"
    mv -f "$file.part" "$file"
  fi
  actual=$(sha256sum "$file" | cut -d' ' -f1)
  if [[ $actual != "$expected" ]]; then
    rm -f "$file"
    echo "ERROR: SHA256 of $name is $actual, deps.toml pins $expected: download deleted" >&2
    return 1
  fi
  echo "SHA256 ok ($name)" >&2
  printf '%s\n' "$file"
}

# A file of the pinned StarBreaker release (windows-x64 | linux-x64 | addon), downloaded once into local/cache/ and
# checked against its SHA256 in deps.toml; prints its path
starbreaker_asset() {
  local kind=$1 release name expected file actual url
  release=$(pin starbreaker release) || return 1
  case $kind in
    windows-x64) name=starbreaker-$release-windows-x86_64.zip ;;
    linux-x64) name=starbreaker-$release-linux-x86_64.tar.gz ;;
    addon) name=starbreaker_addon-$release.zip ;;
    *) echo "ERROR: unknown StarBreaker release file: $kind" >&2; return 1 ;;
  esac
  expected=$(pin starbreaker "sha256-$kind") || return 1
  url=$(pin starbreaker url) || return 1
  mkdir -p local/cache
  file=local/cache/$name
  if [[ ! -f $file ]]; then
    echo "Downloading $name ..." >&2
    curl -fL --retry 3 -o "$file.part" "$url/releases/download/$release/$name"
    mv -f "$file.part" "$file"
  fi
  actual=$(sha256sum "$file" | cut -d' ' -f1)
  if [[ $actual != "$expected" ]]; then
    rm -f "$file"
    echo "ERROR: SHA256 of $name is $actual, deps.toml pins $expected: download deleted" >&2
    return 1
  fi
  echo "SHA256 ok ($name)" >&2
  printf '%s\n' "$file"
}

# The pinned release's binary for a platform (windows | linux) in local/starbreaker-ps/ with its .commit stamp, as
# starbreaker-build writes them; left alone when the stamp is the pin already. A binary of another commit (a build of
# the fork's HEAD) is not replaced: that fails.
install_starbreaker() {
  local platform=$1 out=local/starbreaker-ps bin pinned stamp asset release
  if [[ $platform == windows ]]; then bin=starbreaker.exe; else bin=starbreaker; fi
  pinned=$(pinned_commit) || return 1
  if [[ -f $out/$bin.commit ]]; then
    stamp=$(cut -d' ' -f1 "$out/$bin.commit")
    if [[ $stamp == "$pinned" ]]; then
      echo "StarBreaker $stamp is there ($out/$bin)" >&2
      return 0
    fi
    if [[ -f $out/$bin ]]; then
      echo "ERROR: $out/$bin is $stamp, deps.toml pins $pinned: delete it to take the release" >&2
      return 1
    fi
  fi
  asset=$(starbreaker_asset "$platform-x64") || return 1
  release=$(pin starbreaker release) || return 1
  mkdir -p "$out"
  if [[ $asset == *.zip ]]; then unzip -oq "$asset" -d "$out"; else tar -xzf "$asset" -C "$out"; fi
  [[ -f $out/$bin ]] || { echo "ERROR: $asset holds no $bin" >&2; return 1; }
  printf '%s starbreaker-ps %s (release)\n' "$pinned" "$release" >"$out/$bin.commit"
  if [[ $platform == linux ]]; then cp -f "$out/$bin.commit" "$out/COMMIT"; fi
  echo "StarBreaker $release ($pinned) in $out/$bin" >&2
}

# The add-on of the pinned release, unpacked once into local/starbreaker-addon/<release>/; prints its folder
release_addon() {
  local release dir zip
  release=$(pin starbreaker release) || return 1
  dir=local/starbreaker-addon/$release
  if [[ ! -f $dir/starbreaker_addon/__init__.py ]]; then
    zip=$(starbreaker_asset addon) || return 1
    rm -rf "$dir"
    mkdir -p "$dir"
    unzip -q "$zip" -d "$dir"
    [[ -f $dir/starbreaker_addon/__init__.py ]] || { echo "ERROR: $zip holds no starbreaker_addon/" >&2; return 1; }
  fi
  printf '%s\n' "$dir/starbreaker_addon"
}
