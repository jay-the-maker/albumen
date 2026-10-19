#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Install albumen, albumen-mcp, and albumen-receive into a directory you own.
# Downloads a GitHub release archive, or uses ALBUMEN_ARCHIVE for a local one.
# The archive SHA-256 and the SHA256SUMS inside it are required. A cosign
# bundle is checked when cosign is on PATH. A GitHub attestation is checked
# when gh is on PATH and the archive came from github.com. A missing tool is
# reported. A rejected signature is an error. Nothing is copied until those
# checks pass. This script does not elevate.
set -euo pipefail

repo="${ALBUMEN_REPO:-jay-the-maker/albumen}"
release_base="${ALBUMEN_RELEASE_BASE:-https://github.com/${repo}/releases}"
tag="${ALBUMEN_RELEASE:-latest}"
cosign_issuer="https://token.actions.githubusercontent.com"
cosign_identity='^https://github.com/jay-the-maker/albumen/\.github/workflows/macos-binaries\.yml@.*'

if [[ "$(id -u)" -eq 0 ]]; then
  echo "install.sh installs into your home directory and does not run as root" >&2
  exit 1
fi

os=""
artifact=""
bins=()

detect() {
  local machine
  machine="$(uname -m)"
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW*|MSYS*|CYGWIN*) os=windows ;;
    *)
      echo "unsupported operating system: $(uname -s)" >&2
      exit 1
      ;;
  esac
  case "$os-$machine" in
    linux-x86_64|linux-amd64) artifact=albumen-linux-x86_64 ;;
    linux-aarch64|linux-arm64) artifact=albumen-linux-aarch64 ;;
    macos-arm64|macos-aarch64) artifact=albumen-macos-arm64 ;;
    macos-x86_64) artifact=albumen-macos-x86_64 ;;
    windows-x86_64|windows-amd64) artifact=albumen-windows-x86_64 ;;
    windows-aarch64|windows-arm64) artifact=albumen-windows-aarch64 ;;
    *)
      echo "unsupported platform: $os $machine" >&2
      exit 1
      ;;
  esac
  if [[ "$os" == "windows" ]]; then
    bins=(albumen.exe albumen-mcp.exe albumen-receive.exe)
  else
    bins=(albumen albumen-mcp albumen-receive)
  fi
  if [[ -n "${ALBUMEN_ARTIFACT:-}" ]]; then
    artifact="$ALBUMEN_ARTIFACT"
  fi
}

detect

case "${1:-}" in
  --print-artifact)
    printf '%s\n' "$artifact"
    exit 0
    ;;
  --print-bins)
    printf '%s\n' "${bins[@]}"
    exit 0
    ;;
  "") ;;
  *)
    echo "usage: install.sh [--print-artifact|--print-bins]" >&2
    exit 1
    ;;
esac

file_sha() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print tolower($1)}'
  else
    echo "sha256sum or shasum is required" >&2
    exit 1
  fi
}

abs_file() {
  local dir base
  dir="$(cd "$(dirname "$1")" && pwd)"
  base="$(basename "$1")"
  printf '%s/%s\n' "$dir" "$base"
}

reject_climb() {
  local part
  local old_ifs="$IFS"
  IFS=/
  for part in $1; do
    if [[ "$part" == ".." ]]; then
      echo "refusing a path that climbs out of the home directory" >&2
      exit 1
    fi
  done
  IFS="$old_ifs"
}

install_dest() {
  local dest="$1"
  local home
  dest="${dest%/}"
  reject_climb "$dest"
  home="$(cd "$HOME" && pwd)"
  case "$dest" in
    /*) ;;
    *) dest="$home/$dest" ;;
  esac
  case "$dest/" in
    "$home"/*) ;;
    *)
      echo "refusing to install outside your home directory: $dest" >&2
      exit 1
      ;;
  esac
  mkdir -p "$dest/bin"
  printf '%s\n' "$dest"
}

check_member() {
  case "$1" in
    /*|../*|*/../*|*/..|..)
      echo "refusing archive member $1" >&2
      exit 1
      ;;
  esac
}

verify_checksum_file() {
  local file="$1"
  local sums="$2"
  local expected name actual
  read -r expected name <"$sums" || {
    echo "empty checksum file: $sums" >&2
    exit 1
  }
  expected="${expected//$'\r'/}"
  name="${name//$'\r'/}"
  expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
  name="${name#\*}"
  if [[ "$name" != "$(basename "$file")" ]]; then
    echo "checksum is for ${name}, not $(basename "$file")" >&2
    exit 1
  fi
  actual="$(file_sha "$file")"
  if [[ "$actual" != "$expected" ]]; then
    echo "SHA-256 mismatch for $(basename "$file")" >&2
    echo "  expected $expected" >&2
    echo "  actual   $actual" >&2
    exit 1
  fi
}

verify_inner_sums() {
  local dir="$1"
  local line sum name got seen=" "
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line//$'\r'/}"
    [[ -z "$line" ]] && continue
    sum="${line%% *}"
    name="${line#*"${sum}"}"
    name="${name#"${name%%[![:space:]]*}"}"
    name="${name#\*}"
    sum="$(printf '%s' "$sum" | tr 'A-F' 'a-f')"
    check_member "$name"
    if [[ ! -f "$dir/$name" ]]; then
      echo "archive is missing $name" >&2
      exit 1
    fi
    got="$(file_sha "$dir/$name")"
    if [[ "$got" != "$sum" ]]; then
      echo "SHA-256 mismatch for $name" >&2
      echo "  expected $sum" >&2
      echo "  actual   $got" >&2
      exit 1
    fi
    seen="$seen$name "
  done <"$dir/SHA256SUMS"
  local bin
  for bin in "${bins[@]}"; do
    case "$seen" in
      *" $bin "*) ;;
      *)
        echo "SHA256SUMS does not list $bin" >&2
        exit 1
        ;;
    esac
  done
}

bundle_subject() {
  local bundle="$1"
  local archive="$2"
  local extract="$3"
  local base rest
  base="$(basename "$bundle" .bundle)"
  rest="${base#"${artifact}."}"
  if [[ "$rest" == "$base" ]]; then
    echo "bundle name does not match $artifact: $bundle" >&2
    exit 1
  fi
  if [[ "$rest" == "$(basename "$archive")" ]]; then
    printf '%s\n' "$archive"
    return
  fi
  check_member "$rest"
  if [[ ! -f "$extract/$rest" ]]; then
    echo "bundle subject is not in the archive: $rest" >&2
    exit 1
  fi
  printf '%s\n' "$extract/$rest"
}

verify_bundles() {
  local archive="$1"
  local extract="$2"
  local dir bundle subject
  dir="$(dirname "$archive")"
  shopt -s nullglob
  local bundles=("$dir/${artifact}".*.bundle)
  shopt -u nullglob
  if [[ ${#bundles[@]} -eq 0 ]]; then
    return 1
  fi
  if ! command -v cosign >/dev/null 2>&1; then
    echo "attestation: bundle present, cosign not on PATH; SHA-256 was checked"
    return 0
  fi
  for bundle in "${bundles[@]}"; do
    subject="$(bundle_subject "$bundle" "$archive" "$extract")"
    # The caller tests this function, which turns off errexit for the whole call.
    if ! cosign verify-blob \
      --bundle "$bundle" \
      --certificate-oidc-issuer "$cosign_issuer" \
      --certificate-identity-regexp "$cosign_identity" \
      "$subject"
    then
      echo "attestation: cosign rejected $(basename "$bundle")" >&2
      exit 1
    fi
  done
  echo "attestation: cosign verified ${#bundles[@]} bundle(s)"
  return 0
}

attest_with_gh() {
  local archive="$1"
  local err status
  if ! command -v gh >/dev/null 2>&1; then
    return 1
  fi
  set +e
  err="$(gh attestation verify "$archive" --repo "$repo" 2>&1)"
  status=$?
  set -e
  if [[ "$status" -eq 0 ]]; then
    echo "attestation: gh verified $(basename "$archive")"
    return 0
  fi
  if printf '%s' "$err" | grep -Eiq 'no matching attestations|no attestations|failed to fetch|HTTP 40[13]|authentication|unauthorized|not found'; then
    echo "attestation: gh could not find one; SHA-256 was checked"
    return 0
  fi
  printf '%s\n' "$err" >&2
  echo "attestation: gh rejected $(basename "$archive")" >&2
  exit 1
}

want_gh() {
  if [[ "${ALBUMEN_ATTEST_WITH_GH:-}" == "1" ]]; then
    return 0
  fi
  if [[ -n "${ALBUMEN_ARCHIVE:-}" ]]; then
    return 1
  fi
  case "$release_base" in
    https://github.com/*) ;;
    *) return 1 ;;
  esac
  command -v gh >/dev/null 2>&1
}

download() {
  local url="$1"
  local dest="$2"
  curl -fsSL "$url" -o "$dest"
}

try_download() {
  local url="$1"
  local dest="$2"
  if curl -fsSL "$url" -o "$dest" 2>/dev/null; then
    return 0
  fi
  rm -f "$dest"
  return 1
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
extract="$work/extract"
mkdir -p "$extract"

if [[ -n "${ALBUMEN_ARCHIVE:-}" ]]; then
  archive="$(abs_file "$ALBUMEN_ARCHIVE")"
  sums="${ALBUMEN_ARCHIVE_SHA256:-${archive}.sha256}"
  if [[ ! -f "$archive" || ! -f "$sums" ]]; then
    echo "ALBUMEN_ARCHIVE needs the archive and a sibling .sha256 file" >&2
    exit 1
  fi
else
  if [[ "$tag" == "latest" ]]; then
    prefix="$release_base/latest/download"
  else
    prefix="$release_base/download/$tag"
  fi
  archive="$work/${artifact}.tar.gz"
  sums="$work/${artifact}.tar.gz.sha256"
  download "$prefix/${artifact}.tar.gz" "$archive"
  download "$prefix/${artifact}.tar.gz.sha256" "$sums"
  names=("${bins[@]}" "${artifact}.tar.gz")
  for name in "${names[@]}"; do
    try_download "$prefix/${artifact}.${name}.bundle" "$work/${artifact}.${name}.bundle" || true
  done
fi

verify_checksum_file "$archive" "$sums"

while IFS= read -r member; do
  member="${member//$'\r'/}"
  [[ -z "$member" ]] && continue
  check_member "$member"
done < <(tar -tzf "$archive")

tar -xzf "$archive" -C "$extract"
if [[ ! -f "$extract/SHA256SUMS" ]]; then
  echo "archive has no SHA256SUMS" >&2
  exit 1
fi
verify_inner_sums "$extract"

attested=0
if verify_bundles "$archive" "$extract"; then
  attested=1
fi
if want_gh; then
  if attest_with_gh "$archive"; then
    attested=1
  fi
fi
if [[ "$attested" -eq 0 ]]; then
  echo "attestation: not available; SHA-256 was checked"
fi

if [[ -n "${ALBUMEN_HOME:-}" ]]; then
  dest="$ALBUMEN_HOME"
else
  dest="$HOME/.local/share/albumen"
fi
dest="$(install_dest "$dest")"
for bin in "${bins[@]}"; do
  cp "$extract/$bin" "$dest/bin/$bin"
  chmod 755 "$dest/bin/$bin"
  if [[ "$os" == "macos" ]] && command -v xattr >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$dest/bin/$bin" 2>/dev/null || true
  fi
done
cp "$extract/SHA256SUMS" "$dest/SHA256SUMS"
echo "installed: $dest/bin"
