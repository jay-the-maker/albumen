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

if [[ ! -f "$TARBALL" ]] || ! echo "${SHA256}  ${TARBALL}" | sha256sum -c - >/dev/null 2>&1; then
  curl -fsSL -o "${TARBALL}.partial" "$URL"
  echo "${SHA256}  ${TARBALL}.partial" | sha256sum -c - >&2
  mv "${TARBALL}.partial" "$TARBALL"
fi

# Check the tarball again immediately before extracting so a replaced cache
# file cannot be unpacked.
echo "${SHA256}  ${TARBALL}" | sha256sum -c - >&2

if [[ ! -x "${ROOT}/exiftool" ]] || [[ "$("${ROOT}/exiftool" -ver 2>/dev/null || true)" != "$VERSION" ]]; then
  rm -rf "$ROOT"
  tar -xzf "$TARBALL" -C "$CACHE"
fi

actual="$("${ROOT}/exiftool" -ver)"
if [[ "$actual" != "$VERSION" ]]; then
  echo "exiftool reported ${actual}, expected ${VERSION}" >&2
  exit 1
fi

if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "$ROOT" >>"$GITHUB_PATH"
fi

printf '%s\n' "$ROOT"
