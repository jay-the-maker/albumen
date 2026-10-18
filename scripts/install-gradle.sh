#!/usr/bin/env bash
# Install Gradle 8.11.1 and refuse a zip whose SHA-256 does not match.
# Prints the gradle executable. In GitHub Actions that directory is also
# appended to GITHUB_PATH.
set -euo pipefail

VERSION="8.11.1"
SHA256="f397b287023acdba1e9f6fc5ea72d22dd63669d59ed4a289a29b1a76eee151c6"
URL="https://services.gradle.org/distributions/gradle-${VERSION}-bin.zip"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
ZIP="${CACHE}/gradle-${VERSION}-bin.zip"
ROOT="${CACHE}/gradle-${VERSION}"

if ! command -v unzip >/dev/null 2>&1; then
  echo "unzip is required to install Gradle ${VERSION}" >&2
  exit 1
fi

mkdir -p "$CACHE"

if [[ ! -f "$ZIP" ]] || ! echo "${SHA256}  ${ZIP}" | sha256sum -c - >/dev/null 2>&1; then
  curl -fsSL -o "${ZIP}.partial" "$URL"
  echo "${SHA256}  ${ZIP}.partial" | sha256sum -c - >&2
  mv "${ZIP}.partial" "$ZIP"
fi

echo "${SHA256}  ${ZIP}" | sha256sum -c - >&2

if [[ ! -x "${ROOT}/bin/gradle" ]]; then
  rm -rf "$ROOT"
  unzip -q "$ZIP" -d "$CACHE"
fi

if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "${ROOT}/bin" >>"$GITHUB_PATH"
fi

printf '%s\n' "${ROOT}/bin/gradle"
