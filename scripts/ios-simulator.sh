#!/usr/bin/env bash
# Run the iOS simulator backup checks on a Mac.
# The simulator does not finish a background URLSession, so the test drives
# the foreground upload. This script does not publish an app and it does not
# store a signing identity.
set -euo pipefail

if ! command -v xcodebuild >/dev/null 2>&1; then
  echo "xcodebuild is required. The iOS simulator job runs on the macOS runner." >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required to build albumen-receive." >&2
  exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cargo build --locked --release -p albumen-receive

library=$(mktemp -d)
log=$(mktemp)
password=albumen-ci-not-a-secret
ALBUMEN_RECEIVE_PASSWORD="$password" \
  "$root/target/release/albumen-receive" \
  --apply \
  --to "$library" \
  --user camera \
  --webdav 127.0.0.1:18080 \
  >"$log" 2>&1 &
receiver_pid=$!
cleanup() {
  if kill -0 "$receiver_pid" 2>/dev/null; then
    kill "$receiver_pid" 2>/dev/null || true
    wait "$receiver_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

ready=0
for _ in $(seq 1 50); do
  if grep -q "webdav http://127.0.0.1:18080" "$log"; then
    ready=1
    break
  fi
  if ! kill -0 "$receiver_pid" 2>/dev/null; then
    echo "albumen-receive stopped before it listened" >&2
    cat "$log" >&2
    exit 1
  fi
  sleep 0.2
done
if [[ "$ready" -ne 1 ]]; then
  echo "albumen-receive did not listen on 127.0.0.1:18080" >&2
  cat "$log" >&2
  exit 1
fi

# A newer simulator runtime than this Xcode's SDK rejects simctl privacy.
# Stay on the selected Xcode and boot an iPhone runtime it can control.
udid=$(python3 - <<'PY'
import json, re, subprocess, sys

def version_tuple(text):
    return tuple(int(part) for part in re.findall(r"\d+", text))

sdk = subprocess.check_output(
    ["xcrun", "--sdk", "iphonesimulator", "--show-sdk-version"],
    text=True,
).strip()
sdk_version = version_tuple(sdk)
raw = subprocess.check_output(["xcrun", "simctl", "list", "devices", "available", "-j"])
data = json.loads(raw)
candidates = []
for runtime, devices in data.get("devices", {}).items():
    if "iOS" not in runtime:
        continue
    runtime_version = version_tuple(runtime)
    if not runtime_version or runtime_version > sdk_version:
        continue
    for device in devices:
        if device.get("isAvailable") and str(device.get("name", "")).startswith("iPhone"):
            candidates.append((runtime_version, runtime, device["udid"], device["name"]))
if not candidates:
    sys.exit(f"no iPhone simulator runtime is installed for iOS {sdk}")
candidates.sort()
runtime_version, runtime, udid, name = candidates[-1]
print(f"iOS SDK {sdk}: {name} {runtime}", file=sys.stderr)
print(udid)
PY
)

xcrun simctl boot "$udid" >/dev/null 2>&1 || true
xcrun simctl bootstatus "$udid" -b

derived=$(mktemp -d)
sign=(
  CODE_SIGNING_ALLOWED=YES
  CODE_SIGN_IDENTITY=-
  CODE_SIGNING_REQUIRED=NO
  CODE_SIGN_STYLE=Manual
)
xcodebuild build-for-testing \
  -project "$root/mobile/ios/Albumen.xcodeproj" \
  -scheme AlbumenSimulator \
  -destination "platform=iOS Simulator,id=$udid" \
  -configuration Debug \
  -derivedDataPath "$derived" \
  -parallel-testing-enabled NO \
  "${sign[@]}"

app=$(find "$derived/Build/Products" -name 'AlbumenSimulatorHost.app' -type d -print -quit)
if [[ -z "$app" ]]; then
  echo "AlbumenSimulatorHost.app was not built" >&2
  find "$derived" -name '*.app' -type d >&2 || true
  exit 1
fi
xcrun simctl install "$udid" "$app"
# The test posts a local notification from this host. Granting it avoids the alert.
bundle=photos.albumen.ios.simulator-host
if ! xcrun simctl privacy "$udid" grant notifications "$bundle"; then
  echo "simctl could not grant notifications; writing the simulator permission row" >&2
  python3 - "$HOME/Library/Developer/CoreSimulator/Devices/$udid/data/Library/TCC/TCC.db" "$bundle" <<'PY'
import sqlite3, sys, time
db_path, bundle = sys.argv[1], sys.argv[2]
connection = sqlite3.connect(db_path)
columns = [row[1] for row in connection.execute("PRAGMA table_info(access)")]
if not columns:
    sys.exit(f"no access table in {db_path}")
values = {
    "service": "kTCCServiceUserNotifications",
    "client": bundle,
    "client_type": 0,
    "auth_value": 2,
    "auth_reason": 2,
    "auth_version": 1,
    "flags": 0,
    "last_modified": int(time.time()),
    "indirect_object_identifier": "UNUSED",
}
present = [name for name in values if name in columns]
connection.execute("DELETE FROM access WHERE service = ? AND client = ?", (values["service"], bundle))
connection.execute(
    f"INSERT INTO access ({', '.join(present)}) VALUES ({', '.join('?' for _ in present)})",
    [values[name] for name in present],
)
connection.commit()
PY
fi

xcodebuild test-without-building \
  -project "$root/mobile/ios/Albumen.xcodeproj" \
  -scheme AlbumenSimulator \
  -destination "platform=iOS Simulator,id=$udid" \
  -configuration Debug \
  -derivedDataPath "$derived" \
  -parallel-testing-enabled NO \
  "${sign[@]}"
