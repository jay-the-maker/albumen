#!/usr/bin/env bash
# Install Android command-line tools 13114758 and the pinned platform packages.
# The command-line tools zip is checked with SHA-256 before it is extracted.
# sdkmanager then checks platforms;android-35 and build-tools;35.0.0 against
# Google's repository checksums.
#
# Prints the SDK root. In GitHub Actions, ANDROID_HOME and ANDROID_SDK_ROOT
# are appended to GITHUB_ENV.
set -euo pipefail

VERSION="13114758"
SHA256="7ec965280a073311c339e571cd5de778b9975026cfcbe79f2b1cdcb1e15317ee"
URL="https://dl.google.com/android/repository/commandlinetools-linux-${VERSION}_latest.zip"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
ZIP="${CACHE}/commandlinetools-linux-${VERSION}.zip"
SDK="${CACHE}/android-sdk-${VERSION}"

if ! command -v java >/dev/null 2>&1; then
  echo "java is required to install the Android SDK" >&2
  exit 1
fi
if ! command -v unzip >/dev/null 2>&1; then
  echo "unzip is required to install the Android SDK" >&2
  exit 1
fi

mkdir -p "$CACHE"

if [[ ! -f "$ZIP" ]] || ! echo "${SHA256}  ${ZIP}" | sha256sum -c - >/dev/null 2>&1; then
  curl -fsSL -o "${ZIP}.partial" "$URL"
  echo "${SHA256}  ${ZIP}.partial" | sha256sum -c - >&2
  mv "${ZIP}.partial" "$ZIP"
fi

echo "${SHA256}  ${ZIP}" | sha256sum -c - >&2

if [[ ! -x "${SDK}/cmdline-tools/latest/bin/sdkmanager" ]]; then
  rm -rf "${SDK}/cmdline-tools"
  mkdir -p "${SDK}/cmdline-tools"
  unzip -q "$ZIP" -d "${SDK}/cmdline-tools"
  mv "${SDK}/cmdline-tools/cmdline-tools" "${SDK}/cmdline-tools/latest"
fi

SDKMANAGER="${SDK}/cmdline-tools/latest/bin/sdkmanager"
if [[ ! -d "${SDK}/platforms/android-35" || ! -d "${SDK}/build-tools/35.0.0" ]]; then
  set +o pipefail
  yes | "$SDKMANAGER" --sdk_root="$SDK" --licenses >/dev/null 2>&1 || true
  set -o pipefail
  "$SDKMANAGER" --sdk_root="$SDK" "platforms;android-35" "build-tools;35.0.0" >&2
fi

if [[ -n "${GITHUB_ENV:-}" ]]; then
  echo "ANDROID_HOME=${SDK}" >>"$GITHUB_ENV"
  echo "ANDROID_SDK_ROOT=${SDK}" >>"$GITHUB_ENV"
fi

printf '%s\n' "$SDK"
