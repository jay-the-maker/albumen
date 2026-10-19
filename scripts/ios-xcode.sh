#!/usr/bin/env bash
# Build the iOS host and the background upload extension.
# This runs on the macOS GitHub runner. It does not sign the app and it does
# not publish it to a store.
set -euo pipefail

if ! command -v xcodebuild >/dev/null 2>&1; then
  echo "xcodebuild is required. The iOS app builds on the macOS runner." >&2
  exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
xcodebuild \
  -project "$root/mobile/ios/Albumen.xcodeproj" \
  -scheme Albumen \
  -destination "generic/platform=iOS" \
  -configuration Debug \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGNING_REQUIRED=NO \
  CODE_SIGN_IDENTITY="" \
  build
