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
