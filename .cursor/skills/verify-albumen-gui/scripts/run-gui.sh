#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Start, check and stop one `albumen gui` for verification.
#
#   run-gui.sh start LIB WORK RUN   # serve LIB read-only; state in WORK; pid/url/log in RUN
#   run-gui.sh doctor RUN           # is that instance up, ours, and serving LIB?
#   run-gui.sh stop RUN             # stop only the process this script started
#
# ALBUMEN=/path/to/albumen overrides the binary (default: target/debug/albumen,
# or $CARGO_TARGET_DIR/debug/albumen). ExifTool must be on PATH for "Check photos".
# Each RUN folder is one instance on its own port (--port 0), so runs never share.
set -euo pipefail
cmd=${1:-}; shift || true
bin() {
  if [[ -n "${ALBUMEN:-}" ]]; then echo "$ALBUMEN"; return; fi
  local t=${CARGO_TARGET_DIR:-$(git rev-parse --show-toplevel)/target}
  echo "$t/debug/albumen"
}
case "$cmd" in
  start)
    lib=$1 work=$2 run=$3
    mkdir -p "$run"
    [[ -e "$run/pid" ]] && kill -0 "$(cat "$run/pid")" 2>/dev/null && { echo "already running: $(cat "$run/url")"; exit 0; }
    b=$(bin); [[ -x "$b" ]] || { echo "no albumen binary at $b (cargo build --locked -p albumen)"; exit 2; }
    "$b" gui --library "$lib" --work "$work" --port 0 --no-open >"$run/gui.log" 2>&1 &
    echo $! >"$run/pid"
    for _ in $(seq 1 300); do
      url=$(sed -n 's/^albumen gui: //p' "$run/gui.log" | head -1)
      if [[ -n "$url" ]] && curl -fsS -o /dev/null "${url}api/library"; then
        echo "$url" >"$run/url"; echo "$lib" >"$run/library"; echo "ready: $url"; exit 0
      fi
      kill -0 "$(cat "$run/pid")" 2>/dev/null || { echo "albumen gui exited:"; cat "$run/gui.log"; exit 1; }
      sleep 0.1
    done
    echo "not ready after 30 s"; cat "$run/gui.log"; exit 1 ;;
  doctor)
    run=$1
    pid=$(cat "$run/pid"); url=$(cat "$run/url"); lib=$(cat "$run/library")
    kill -0 "$pid" 2>/dev/null || { echo "FAIL: pid $pid is not running"; exit 1; }
    body=$(curl -fsS "${url}api/library") || { echo "FAIL: ${url}api/library did not answer"; exit 1; }
    python3 - "$body" "$lib" <<'PY'
import json, os, sys
v = json.loads(sys.argv[1]); want = os.path.realpath(sys.argv[2])
got = os.path.realpath(v["library"])
ok = got == want
print(("ok" if ok else "FAIL") + f": serving {got}; {len(v['images'])} photos shown of {v['files']} files; skipped {v.get('skipped', {}).get('count', 'Unknown')}")
sys.exit(0 if ok else 1)
PY
    ;;
  stop)
    run=$1
    if [[ -e "$run/pid" ]]; then kill "$(cat "$run/pid")" 2>/dev/null || true; rm -f "$run/pid"; fi
    echo "stopped" ;;
  *) sed -n '3,12p' "$0"; exit 2 ;;
esac
