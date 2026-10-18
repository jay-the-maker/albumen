#!/usr/bin/env bash
# Compile the Android camera-roll library and run its JVM unit tests.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
sdk=$("$root/scripts/install-android-sdk.sh" | tail -n 1)
gradle=$("$root/scripts/install-gradle.sh" | tail -n 1)
export ANDROID_HOME="$sdk"
export ANDROID_SDK_ROOT="$sdk"
printf 'sdk.dir=%s\n' "$sdk" >"$root/mobile/android/local.properties"

cd "$root/mobile/android"
"$gradle" :backup:testDebugUnitTest --no-daemon --stacktrace
