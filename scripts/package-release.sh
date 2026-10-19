#!/usr/bin/env bash
# Pack albumen, albumen-mcp, and albumen-receive into <artifact>.tar.gz.
# SHA256SUMS is inside the archive. <artifact>.tar.gz.sha256 sits beside it.
# Usage: scripts/package-release.sh <target-triple> <artifact-name>
set -euo pipefail

target="${1:?target triple}"
artifact="${2:?artifact name}"
root="$(cd "$(dirname "$0")/.." && pwd)"
bin_dir="${ALBUMEN_RELEASE_BIN_DIR:-$root/target/$target/release}"
out_dir="${ALBUMEN_RELEASE_OUT_DIR:-${GITHUB_WORKSPACE:-$root}}"

# Git Bash leaves GITHUB_WORKSPACE as D:\... . GNU tar reads the colon as a
# remote host and never writes the archive. A POSIX path stays on this machine.
posix_path() {
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      if ! command -v cygpath >/dev/null 2>&1; then
        echo "cygpath is required to package on Windows" >&2
        exit 1
      fi
      cygpath -u "$1"
      ;;
    *)
      printf '%s\n' "$1"
      ;;
  esac
}

bin_dir="$(posix_path "$bin_dir")"
out_dir="$(posix_path "$out_dir")"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

copy_bin() {
  local name="$1"
  if [ -f "$bin_dir/${name}.exe" ]; then
    cp "$bin_dir/${name}.exe" "$stage/${name}.exe"
  elif [ -f "$bin_dir/$name" ]; then
    cp "$bin_dir/$name" "$stage/$name"
  else
    echo "missing binary: $name in $bin_dir" >&2
    exit 1
  fi
}

checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

copy_bin albumen
copy_bin albumen-mcp
copy_bin albumen-receive

mkdir -p "$out_dir"
(
  cd "$stage"
  # Stable order: the three binaries, then SHA256SUMS.
  set -- *
  checksum "$@" > SHA256SUMS
  COPYFILE_DISABLE=1 tar -czf "$out_dir/${artifact}.tar.gz" SHA256SUMS "$@"
  for file in "$@"; do
    cp "$file" "$out_dir/"
  done
)
(
  cd "$out_dir"
  checksum "${artifact}.tar.gz" > "${artifact}.tar.gz.sha256"
)
