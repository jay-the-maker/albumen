#!/usr/bin/env bash
# Run the camera-roll instrumented tests on an emulator that is already booted.
# GitHub Actions boots that emulator with reactivecircus/android-emulator-runner.
# This script does not start a second emulator and does not publish an APK.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)

if [[ -z "${ANDROID_HOME:-}" ]]; then
  echo "ANDROID_HOME is required. The emulator runner sets it after the AVD boots." >&2
  exit 1
fi
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"

sdkmanager=""
for candidate in \
  "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" \
  "$ANDROID_HOME/cmdline-tools/bin/sdkmanager"
do
  if [[ -x "$candidate" ]]; then
    sdkmanager=$candidate
    break
  fi
done
if [[ -z "$sdkmanager" ]]; then
  echo "sdkmanager was not found under ANDROID_HOME=$ANDROID_HOME" >&2
  exit 1
fi

# compileSdk is 35. The runner's system image can be an older API.
if [[ ! -d "$ANDROID_HOME/platforms/android-35" || ! -d "$ANDROID_HOME/build-tools/35.0.0" ]]; then
  set +o pipefail
  yes | "$sdkmanager" --sdk_root="$ANDROID_HOME" --licenses >/dev/null 2>&1 || true
  set -o pipefail
  "$sdkmanager" --sdk_root="$ANDROID_HOME" "platforms;android-35" "build-tools;35.0.0"
fi

if ! adb devices | tr -d '\r' | grep -q 'emulator.*device$'; then
  echo "no booted emulator is attached" >&2
  adb devices >&2
  exit 1
fi

gradle=$("$root/scripts/install-gradle.sh" | tail -n 1)
printf 'sdk.dir=%s\n' "$ANDROID_HOME" >"$root/mobile/android/local.properties"
cd "$root/mobile/android"
"$gradle" :backup:connectedDebugAndroidTest :host:connectedDebugAndroidTest --no-daemon --stacktrace
