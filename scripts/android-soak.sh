#!/usr/bin/env bash
# Repeated camera-roll backup cycles against a loopback receiver.
# Faults: receiver down, network drop, kill mid-upload, SHA-256 mismatch.
# The Gradle test writes build/backup-soak-result.txt and fails on a silent miss.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
sdk=$("$root/scripts/install-android-sdk.sh" | tail -n 1)
gradle=$("$root/scripts/install-gradle.sh" | tail -n 1)
export ANDROID_HOME="$sdk"
export ANDROID_SDK_ROOT="$sdk"
printf 'sdk.dir=%s\n' "$sdk" >"$root/mobile/android/local.properties"

cd "$root/mobile/android"
"$gradle" :backup:testDebugUnitTest --tests photos.albumen.backup.BackupSoakTest --no-daemon --stacktrace --console=plain

result="$root/mobile/android/backup/build/backup-soak-result.txt"
if [[ ! -f "$result" ]]; then
  echo "backup soak did not write $result" >&2
  exit 1
fi
cat "$result"
grep -q 'silent_failures=0' "$result"
