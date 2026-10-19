#!/usr/bin/env bash
# Compile the Android camera-roll library and host app, and run their unit tests.
# Robolectric runs inside :backup:testDebugUnitTest. The emulator job is separate.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
sdk=$("$root/scripts/install-android-sdk.sh" | tail -n 1)
gradle=$("$root/scripts/install-gradle.sh" | tail -n 1)
export ANDROID_HOME="$sdk"
export ANDROID_SDK_ROOT="$sdk"
printf 'sdk.dir=%s\n' "$sdk" >"$root/mobile/android/local.properties"

cd "$root/mobile/android"
"$gradle" :backup:testDebugUnitTest :host:testDebugUnitTest :host:assembleDebug --no-daemon --stacktrace
