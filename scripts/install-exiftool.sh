#!/usr/bin/env bash
# Install one ExifTool release and refuse a tarball whose SHA-256 does not match.
#
# The 12.76 tarball that generated the fixture bytes is no longer published
# (exiftool.org and SourceForge both return 404). The comparison gate pins
# Image-ExifTool 13.59, which is still published, and uses that same binary
# on the source and the copy. Fixture bytes are not regenerated.
#
# Prints the directory that contains the exiftool executable. In GitHub Actions
# that directory is also appended to GITHUB_PATH.
set -euo pipefail

check_sha256() {
  local sum="$1"
  local file="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    echo "${sum}  ${file}" | sha256sum -c - >&2
  elif command -v shasum >/dev/null 2>&1; then
    echo "${sum}  ${file}" | shasum -a 256 -c - >&2
  else
    echo "sha256sum or shasum is required" >&2
    exit 1
  fi
}

sha256_ok() {
  local sum="$1"
  local file="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    echo "${sum}  ${file}" | sha256sum -c - >/dev/null 2>&1
  else
    echo "${sum}  ${file}" | shasum -a 256 -c - >/dev/null 2>&1
  fi
}

VERSION="13.59"
SHA256="668ea3acececb7235fbd0f4900e72d5f12c9b07e5c778fd36cb1e9b5828fd65a"
URL="https://downloads.sourceforge.net/project/exiftool/Image-ExifTool-${VERSION}.tar.gz"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
ROOT="${CACHE}/Image-ExifTool-${VERSION}"
TARBALL="${CACHE}/Image-ExifTool-${VERSION}.tar.gz"

if ! command -v perl >/dev/null 2>&1; then
  echo "perl is required to run ExifTool ${VERSION}" >&2
  exit 1
fi

mkdir -p "$CACHE"

if [[ ! -f "$TARBALL" ]] || ! sha256_ok "$SHA256" "$TARBALL"; then
  curl -fsSL -o "${TARBALL}.partial" "$URL"
  check_sha256 "$SHA256" "${TARBALL}.partial"
  mv "${TARBALL}.partial" "$TARBALL"
fi

# Check the tarball again immediately before extracting so a replaced cache
# file cannot be unpacked.
check_sha256 "$SHA256" "$TARBALL"

windows_perl=0
case "${OS_NAME:-$(uname -s)}" in
  MINGW*|MSYS*|CYGWIN*) windows_perl=1 ;;
esac

if [[ "$windows_perl" -eq 1 ]]; then
  if [[ ! -f "${ROOT}/exiftool" ]] || [[ "$(perl "${ROOT}/exiftool" -ver 2>/dev/null || true)" != "$VERSION" ]]; then
    rm -rf "$ROOT"
    tar -xzf "$TARBALL" -C "$CACHE"
  fi
  # Rust on Windows finds exiftool.cmd, not an extensionless Perl script.
  cat > "${ROOT}/exiftool.cmd" <<'EOF'
@echo off
perl "%~dp0exiftool" %*
EOF
  actual="$(perl "${ROOT}/exiftool" -ver)"
else
  if [[ ! -x "${ROOT}/exiftool" ]] || [[ "$("${ROOT}/exiftool" -ver 2>/dev/null || true)" != "$VERSION" ]]; then
    rm -rf "$ROOT"
    tar -xzf "$TARBALL" -C "$CACHE"
  fi
  actual="$("${ROOT}/exiftool" -ver)"
fi
if [[ "$actual" != "$VERSION" ]]; then
  echo "exiftool reported ${actual}, expected ${VERSION}" >&2
  exit 1
fi

if [[ -n "${GITHUB_PATH:-}" ]]; then
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$ROOT" >>"$GITHUB_PATH"
  else
    echo "$ROOT" >>"$GITHUB_PATH"
  fi
fi

printf '%s\n' "$ROOT"
