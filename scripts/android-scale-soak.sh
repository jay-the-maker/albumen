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
set +e
"$gradle" :backup:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=photos.albumen.backup.ScaleSoakTest \
  -Pandroid.testInstrumentationRunnerArguments.albumenSoakItems="$items" \
  -Pandroid.testInstrumentationRunnerArguments.albumenSoakShard="$shard" \
  -Pandroid.testInstrumentationRunnerArguments.albumenSoakShards="$shards" \
  -Pandroid.testInstrumentationRunnerArguments.albumenSoakSeed="$seed" \
  --no-daemon --stacktrace --console=plain
status=$?
set -e

dest="$root/build/backup-scale-soak"
mkdir -p "$dest"
remote="/sdcard/Android/data/photos.albumen.backup.test/files/backup-scale-soak"
if adb shell "ls $remote/result.txt" >/dev/null 2>&1; then
  adb pull "$remote/result.txt" "$dest/android-shard-${shard}-result.txt"
  adb pull "$remote/faults.log" "$dest/android-shard-${shard}-faults.log"
  adb pull "$remote/timeline.csv" "$dest/android-shard-${shard}-timeline.csv"
else
  pkg=$(adb shell pm list packages | tr -d '\r' | awk -F: '/photos\.albumen\.backup\.test$/ {print $2; exit}')
  if [[ -n "$pkg" ]]; then
    adb exec-out run-as "$pkg" cat files/backup-scale-soak/result.txt >"$dest/android-shard-${shard}-result.txt" || true
    adb exec-out run-as "$pkg" cat files/backup-scale-soak/faults.log >"$dest/android-shard-${shard}-faults.log" || true
    adb exec-out run-as "$pkg" cat files/backup-scale-soak/timeline.csv >"$dest/android-shard-${shard}-timeline.csv" || true
  fi
fi
if [[ -s "$dest/android-shard-${shard}-result.txt" ]]; then
  cat "$dest/android-shard-${shard}-result.txt"
elif [[ "$status" -eq 0 ]]; then
  echo "scale soak did not write a result" >&2
  adb logcat -d -s albumen-scale-soak:I >&2 || true
  exit 1
fi
if [[ "$status" -eq 0 ]] && ! grep -q 'silent_failures=0' "$dest/android-shard-${shard}-result.txt"; then
  echo "scale soak reported a silent failure" >&2
  exit 1
fi
exit "$status"
