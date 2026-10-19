#!/usr/bin/env bash
# Run the Android camera-roll instrumented test on an API 30 emulator.
# The system image is installed by sdkmanager, which checks Google's checksums.
# The command-line tools zip is checked by scripts/install-android-sdk.sh.
# This does not publish an APK to a store.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
if [[ ! -e /dev/kvm ]]; then
  echo "KVM is required for the camera-roll emulator. /dev/kvm is missing." >&2
  exit 1
fi
if [[ ! -w /dev/kvm ]]; then
  sudo chmod 666 /dev/kvm
fi

if ! command -v xkbcomp >/dev/null 2>&1; then
  sudo apt-get update
  sudo apt-get install -y libnss3 libx11-6 libxcomposite1 libxcursor1 libxi6 libxtst6 libpulse0
fi

sdk=$("$root/scripts/install-android-sdk.sh" | tail -n 1)
gradle=$("$root/scripts/install-gradle.sh" | tail -n 1)
export ANDROID_HOME="$sdk"
export ANDROID_SDK_ROOT="$sdk"
export PATH="$sdk/platform-tools:$sdk/emulator:$PATH"
printf 'sdk.dir=%s\n' "$sdk" >"$root/mobile/android/local.properties"

SDKMANAGER="$sdk/cmdline-tools/latest/bin/sdkmanager"
AVDMANAGER="$sdk/cmdline-tools/latest/bin/avdmanager"
IMAGE="system-images;android-30;aosp_atd;x86_64"
if [[ ! -d "$sdk/system-images/android-30/aosp_atd/x86_64" || ! -x "$sdk/emulator/emulator" ]]; then
  set +o pipefail
  yes | "$SDKMANAGER" --sdk_root="$sdk" --licenses >/dev/null 2>&1 || true
  set -o pipefail
  "$SDKMANAGER" --sdk_root="$sdk" \
    "platforms;android-30" \
    "platform-tools" \
    "emulator" \
    "$IMAGE"
fi

printf 'no\n' | "$AVDMANAGER" create avd --force --name albumen --package "$IMAGE"

log=/tmp/albumen-emulator.log
"$sdk/emulator/emulator" \
  -avd albumen \
  -no-window \
  -no-audio \
  -no-boot-anim \
  -no-snapshot \
  -no-metrics \
  -gpu swiftshader_indirect \
  -accel on \
  -memory 2048 \
  >"$log" 2>&1 &
emu_pid=$!
cleanup() {
  if kill -0 "$emu_pid" 2>/dev/null; then
    kill "$emu_pid" 2>/dev/null || true
    wait "$emu_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

adb wait-for-device
booted=0
for _ in $(seq 1 90); do
  if ! kill -0 "$emu_pid" 2>/dev/null; then
    echo "emulator exited before boot" >&2
    cat "$log" >&2
    exit 1
  fi
  boot=$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')
  if [[ "$boot" == "1" ]]; then
    booted=1
    break
  fi
  sleep 2
done
if [[ "$booted" != "1" ]]; then
  echo "emulator did not finish booting" >&2
  cat "$log" >&2
  exit 1
fi

adb shell input keyevent 82 || true
cd "$root/mobile/android"
"$gradle" :backup:connectedDebugAndroidTest --no-daemon --stacktrace
