#!/usr/bin/env bash
# Android 14 Select photos on an emulator that is already booted (API 34+).
# Runs SelectedPhotosInstrumentedTest alone on a fresh install, before the
# other connected tests grant full media access. Gradle uninstalls the test
# APK when the run ends. CI calls this before android-emulator-connected.sh.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)

if [[ -z "${ANDROID_HOME:-}" ]]; then
  echo "ANDROID_HOME is required. The emulator runner sets it after the AVD boots." >&2
  exit 1
fi
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"

api=$(adb shell getprop ro.build.version.sdk | tr -d '\r')
if [[ "$api" -lt 34 ]]; then
  echo "API $api has no Select photos; nothing to run."
  exit 0
fi

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
# compileSdk is 35. The runner's system image can be an older API.
if [[ ! -d "$ANDROID_HOME/platforms/android-35" || ! -d "$ANDROID_HOME/build-tools/35.0.0" ]]; then
  if [[ -z "$sdkmanager" ]]; then
    echo "sdkmanager was not found under ANDROID_HOME=$ANDROID_HOME" >&2
    exit 1
  fi
  set +o pipefail
  yes | "$sdkmanager" --sdk_root="$ANDROID_HOME" --licenses >/dev/null 2>&1 || true
  set -o pipefail
  "$sdkmanager" --sdk_root="$ANDROID_HOME" "platforms;android-35" "build-tools;35.0.0"
fi

gradle=$("$root/scripts/install-gradle.sh" | tail -n 1)
printf 'sdk.dir=%s\n' "$ANDROID_HOME" >"$root/mobile/android/local.properties"
cd "$root/mobile/android"
"$gradle" :backup:connectedDebugAndroidTest --no-daemon --stacktrace \
  "-Pandroid.testInstrumentationRunnerArguments.class=photos.albumen.backup.SelectedPhotosInstrumentedTest"
