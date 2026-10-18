#!/usr/bin/env bash
# Install one rclone release and refuse a zip whose SHA-256 does not match.
#
# rclone is the transport for SFTP, WebDAV, and Backblaze B2. Albumen does not
# vendor rclone source. This downloads a released binary and runs it as a
# separate process. Prints the directory that contains the rclone executable.
# In GitHub Actions that directory is also appended to GITHUB_PATH.
set -euo pipefail

VERSION="v1.75.2"
SHA256="349ac8fba6ff65d6247043f1750cdcb518ec5d500ef91463a10d37c0ccdf3702"
URL="https://github.com/rclone/rclone/releases/download/${VERSION}/rclone-${VERSION}-linux-amd64.zip"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
ROOT="${CACHE}/rclone-${VERSION}-linux-amd64"
ZIP="${CACHE}/rclone-${VERSION}-linux-amd64.zip"

mkdir -p "$CACHE"

if [[ ! -f "$ZIP" ]] || ! echo "${SHA256}  ${ZIP}" | sha256sum -c - >/dev/null 2>&1; then
  curl -fsSL -o "${ZIP}.partial" "$URL"
  echo "${SHA256}  ${ZIP}.partial" | sha256sum -c - >&2
  mv "${ZIP}.partial" "$ZIP"
fi

echo "${SHA256}  ${ZIP}" | sha256sum -c - >&2

if [[ ! -x "${ROOT}/rclone" ]] || [[ "$("${ROOT}/rclone" version 2>/dev/null | awk 'NR==1{print $2}')" != "$VERSION" ]]; then
  rm -rf "$ROOT"
  unzip -q "$ZIP" -d "$CACHE"
fi

chmod +x "${ROOT}/rclone"
echo "${ROOT}"

if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "${ROOT}" >> "${GITHUB_PATH}"
fi
