#!/usr/bin/env bash
# One shard of the seeded backup soak on an emulator that is already booted.
# Synthetic items only. The camera roll is not opened. This does not publish an APK.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
items=${ALBUMEN_SOAK_ITEMS:?ALBUMEN_SOAK_ITEMS is required}
shard=${ALBUMEN_SOAK_SHARD:-0}
shards=${ALBUMEN_SOAK_SHARDS:-1}
seed=${ALBUMEN_SOAK_SEED:-1}

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
# connectedDebugAndroidTest uninstalls the test package before this script can
# read the result files. Install, instrument, then read while the package remains.
"$gradle" :backup:installDebugAndroidTest --no-daemon --stacktrace --console=plain
pkg=$(adb shell pm list packages photos.albumen.backup | tr -d '\r' | awk -F: '/\.test$/ {print $2; exit}')
if [[ -z "$pkg" ]]; then
  echo "the backup test package was not installed" >&2
  adb shell pm list packages photos.albumen.backup >&2 || true
  exit 1
fi

dest="$root/build/backup-scale-soak"
mkdir -p "$dest"
adb logcat -c || true
set +e
adb shell am instrument -w \
  -e class photos.albumen.backup.ScaleSoakTest \
  -e albumenSoakItems "$items" \
  -e albumenSoakShard "$shard" \
  -e albumenSoakShards "$shards" \
  -e albumenSoakSeed "$seed" \
  "$pkg/androidx.test.runner.AndroidJUnitRunner" | tr -d '\r' | tee "$dest/android-shard-${shard}-instrument.txt"
status=${PIPESTATUS[0]}
set -e
if ! grep -q 'OK (' "$dest/android-shard-${shard}-instrument.txt"; then
  status=1
fi

pull_app_file() {
  local name=$1
  local out=$2
  adb exec-out run-as "$pkg" cat "files/backup-scale-soak/$name" 2>/dev/null | tr -d '\r' >"$out" || true
  if [[ ! -s "$out" ]]; then
    adb shell run-as "$pkg" cat "files/backup-scale-soak/$name" 2>/dev/null | tr -d '\r' >"$out" || true
  fi
}
pull_app_file result.txt "$dest/android-shard-${shard}-result.txt"
pull_app_file faults.log "$dest/android-shard-${shard}-faults.log"
pull_app_file timeline.csv "$dest/android-shard-${shard}-timeline.csv"
if ! grep -q '^backup-scale-soak ' "$dest/android-shard-${shard}-result.txt" 2>/dev/null; then
  adb logcat -d -s albumen-scale-soak:I | tr -d '\r' \
    | sed -n 's/.*\(backup-scale-soak platform=.*\)/\1/p' \
    | tail -n 1 >"$dest/android-shard-${shard}-result.txt" || true
fi
if ! grep -q '^backup-scale-soak ' "$dest/android-shard-${shard}-result.txt" 2>/dev/null; then
  echo "scale soak did not write a result for $pkg" >&2
  adb logcat -d -s albumen-scale-soak:I >&2 || true
  exit 1
fi
cat "$dest/android-shard-${shard}-result.txt"
if ! grep -q 'silent_failures=0' "$dest/android-shard-${shard}-result.txt"; then
  echo "scale soak reported a silent failure" >&2
  exit 1
fi
exit "$status"
