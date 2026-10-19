#!/usr/bin/env bash
# Install one rclone release and refuse a zip whose SHA-256 does not match.
#
# rclone is the transport for SFTP, WebDAV, and Backblaze B2. Albumen does not
# vendor rclone source. This downloads a released binary and runs it as a
# separate process. Prints the directory that contains the rclone executable.
# In GitHub Actions that directory is also appended to GITHUB_PATH.
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

VERSION="v1.75.2"
OS_NAME="$(uname -s)"
ARCH_NAME="$(uname -m)"
case "${OS_NAME}-${ARCH_NAME}" in
  Linux-x86_64|Linux-amd64)
    TRIPLE="linux-amd64"
    SHA256="349ac8fba6ff65d6247043f1750cdcb518ec5d500ef91463a10d37c0ccdf3702"
    ;;
  Darwin-arm64)
    TRIPLE="osx-arm64"
    SHA256="610ab7d48615135dfc60b763858b11a426a0d3d1160c1bb76fb0ae6011b02edd"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    TRIPLE="windows-amd64"
    SHA256="8a9db2bad1dc79a65b3348fd857069598e2aab7cf4d118b77b1df23055aff818"
    ;;
  *)
    echo "Docker is not used. No pinned rclone binary for ${OS_NAME}-${ARCH_NAME}." >&2
    exit 0
    ;;
esac

URL="https://github.com/rclone/rclone/releases/download/${VERSION}/rclone-${VERSION}-${TRIPLE}.zip"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
ROOT="${CACHE}/rclone-${VERSION}-${TRIPLE}"
ZIP="${CACHE}/rclone-${VERSION}-${TRIPLE}.zip"

mkdir -p "$CACHE"

if [[ ! -f "$ZIP" ]] || ! sha256_ok "$SHA256" "$ZIP"; then
  curl -fsSL -o "${ZIP}.partial" "$URL"
  check_sha256 "$SHA256" "${ZIP}.partial"
  mv "${ZIP}.partial" "$ZIP"
fi

check_sha256 "$SHA256" "$ZIP"

RCLONE_BIN="${ROOT}/rclone"
case "${OS_NAME}" in
  MINGW*|MSYS*|CYGWIN*) RCLONE_BIN="${ROOT}/rclone.exe" ;;
esac

if [[ ! -x "${RCLONE_BIN}" ]] || [[ "$("${RCLONE_BIN}" version 2>/dev/null | awk 'NR==1{print $2}')" != "$VERSION" ]]; then
  rm -rf "$ROOT"
  unzip -q "$ZIP" -d "$CACHE"
fi

chmod +x "${RCLONE_BIN}"
if [[ "$OS_NAME" == "Darwin" ]]; then
  xattr -dr com.apple.quarantine "$ROOT" 2>/dev/null || true
fi
echo "${ROOT}"

if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "${ROOT}" >> "${GITHUB_PATH}"
fi
