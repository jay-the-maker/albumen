#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Prove default-config scan, index, and search make no network connection.
#
# Index is the process-local metadata cache. There is no on-disk catalog and
# no index subcommand. The index run is albumen-mcp, with export and upload
# left off, calling search with a metadata filter so ExifTool reads the files.
# The search run is a second default-config process calling search by name.
#
# Linux watches strace -f -e trace=network and fails on any IPv4 or IPv6
# socket, connect, send, or bind, including DNS and a connect from a loopback
# bind toward an external address. macOS uses sandbox-exec to deny network*
# and the mDNSResponder lookup. A canary has to be blocked before the real
# commands are trusted. The library is synthetic.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
checker="$root/scripts/check-zero-network.py"

if ! command -v python3 >/dev/null 2>&1; then
  echo "zero-network: need python3" >&2
  exit 1
fi
python3 "$checker" --self-test

export PATH="$(scripts/install-exiftool.sh):$PATH"
if [[ ! -x target/debug/albumen || ! -x target/debug/albumen-mcp ]]; then
  cargo build --locked -p albumen -p albumen-mcp
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/library" "$work/out" "$work/home" "$work/tmp"
printf 'albumen synthetic original. Not a photograph.\n' >"$work/library/IMG_0001.jpg"
cat >"$work/library/IMG_0001.jpg.xmp" <<'EOF'
<?xpacket begin="" id="albumen-synthetic"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">synthetic caption</rdf:li></rdf:Alt></dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
EOF
hash_before="$(python3 - "$work/library/IMG_0001.jpg" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
mtime_before="$(python3 - "$work/library/IMG_0001.jpg" <<'PY'
import os, sys
print(os.stat(sys.argv[1]).st_mtime_ns)
PY
)"

python3 - "$work/index.bin" "$work/search.bin" <<'PY'
import json, sys

def frame(message):
    body = json.dumps(message).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body

def write(path, call):
    messages = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": call},
    ]
    open(path, "wb").write(b"".join(frame(message) for message in messages))

index_path, search_path = sys.argv[1:]
write(index_path, {"name": "search", "arguments": {"camera": "synthetic-index"}})
write(search_path, {"name": "search", "arguments": {"query": "IMG_0001"}})
PY

base_env=(
  env -i
  PATH="$PATH"
  HOME="$work/home"
  TMPDIR="$work/tmp"
  LANG=C.UTF-8
)
albumen="$root/target/debug/albumen"
mcp="$root/target/debug/albumen-mcp"

assert_source_unchanged() {
  hash_after="$(python3 - "$work/library/IMG_0001.jpg" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
  mtime_after="$(python3 - "$work/library/IMG_0001.jpg" <<'PY'
import os, sys
print(os.stat(sys.argv[1]).st_mtime_ns)
PY
)"
  if [[ "$hash_after" != "$hash_before" || "$mtime_after" != "$mtime_before" ]]; then
    echo "zero-network: source bytes or mtime changed" >&2
    exit 1
  fi
}

check_mcp() {
  local label="$1"
  local stdout="$2"
  local expect_path="${3:-}"
  python3 - "$label" "$stdout" "$expect_path" <<'PY'
import json, sys
label, path, expect = sys.argv[1:]
data = open(path, "rb").read()
messages = []
while data:
    if not data.lower().startswith(b"content-length:"):
        break
    header, _, rest = data.partition(b"\r\n\r\n")
    length = int(header.decode().split(":", 1)[1].strip())
    body, data = rest[:length], rest[length:]
    messages.append(json.loads(body))
calls = [message for message in messages if message.get("id") == 2]
if len(calls) != 1:
    raise SystemExit(f"{label}: missing tools/call result\n{messages!r}")
result = calls[0].get("result", {})
if result.get("isError"):
    raise SystemExit(f"{label}: {result}")
text = json.dumps(result)
if expect and expect not in text:
    raise SystemExit(f"{label}: result did not contain {expect}\n{text}")
print(f"zero-network: {label} completed")
PY
}

run_linux() {
  if ! command -v strace >/dev/null 2>&1; then
    echo "zero-network: need strace" >&2
    exit 1
  fi
  trace_cmd() {
    local name="$1"
    shift
    if ! strace -f -yy -s 160 -e trace=network -o "$work/$name.strace" \
      "${base_env[@]}" "$@" >"$work/$name.stdout" 2>"$work/$name.stderr"
    then
      echo "zero-network: $name failed" >&2
      cat "$work/$name.stderr" >&2
      exit 1
    fi
  }
  cat >"$work/canary.py" <<'PY'
import socket, sys
kind = sys.argv[1]
if kind == "external":
    sock = socket.socket()
    sock.settimeout(0.3)
    sock.connect(("203.0.113.5", 9))
elif kind == "dns":
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.sendto(b"\0", ("127.0.0.1", 53))
    sock.sendto(b"\0", ("203.0.113.5", 53))
elif kind == "loopback-external":
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(0.3)
    sock.connect(("203.0.113.5", 9))
else:
    raise SystemExit(f"unknown canary {kind}")
PY
  set +e
  strace -f -yy -s 160 -e trace=network -o "$work/canary.strace" \
    python3 "$work/canary.py" external >/dev/null 2>&1
  strace -f -yy -s 160 -e trace=network -o "$work/canary-dns.strace" \
    python3 "$work/canary.py" dns >/dev/null 2>&1
  strace -f -yy -s 160 -e trace=network -o "$work/canary-lo.strace" \
    python3 "$work/canary.py" loopback-external >/dev/null 2>&1
  set -e
  cat "$work/canary.strace" "$work/canary-dns.strace" "$work/canary-lo.strace" >"$work/canary-all.strace"
  python3 "$checker" --expect-attempts \
    --contains 203.0.113.5 --contains 127.0.0.1 --contains 'htons(53)' \
    "$work/canary-all.strace"

  trace_cmd scan "$albumen" scan "$work/library" --manifest "$work/out/scan.jsonl"
  grep -q 'scanned ' "$work/scan.stdout"
  python3 "$checker" --expect-clean "$work/scan.strace"

  trace_cmd index "$mcp" --library "$work/library" <"$work/index.bin"
  check_mcp index "$work/index.stdout"
  python3 "$checker" --expect-clean "$work/index.strace"

  trace_cmd search "$mcp" --library "$work/library" <"$work/search.bin"
  check_mcp search "$work/search.stdout" "IMG_0001.jpg"
  python3 "$checker" --expect-clean "$work/search.strace"

  assert_source_unchanged
  echo "zero-network: linux strace -f -e trace=network saw no AF_INET or AF_INET6 socket, connect, send, or bind from scan, index, or search"
}

sandbox_canary() {
  local kind="$1"
  local status=0
  set +e
  "${base_env[@]}" sandbox-exec -f "$work/sandbox.sb" \
    python3 "$work/canary.py" "$kind" >"$work/canary-$kind.out" 2>"$work/canary-$kind.err"
  status=$?
  set -e
  if [[ "$status" -eq 137 || "$status" -eq 9 ]]; then
    echo "zero-network: macOS sandbox killed the $kind canary"
    return
  fi
  if grep -Eiq 'not permitted|permission denied|operation not permitted' "$work/canary-$kind.err"; then
    echo "zero-network: macOS sandbox denied the $kind canary"
    return
  fi
  echo "zero-network: macOS sandbox did not block the $kind canary (status $status)" >&2
  cat "$work/canary-$kind.err" >&2
  exit 1
}

run_macos() {
  if ! command -v sandbox-exec >/dev/null 2>&1; then
    echo "zero-network: sandbox-exec is not on this Mac" >&2
    exit 1
  fi
  cat >"$work/sandbox.sb" <<EOF
(version 1)
(deny default)
(allow process*)
(allow signal)
(allow sysctl-read)
(allow mach*)
(allow ipc*)
(deny mach-lookup (global-name "com.apple.mDNSResponder"))
(deny mach-lookup (global-name "com.apple.mDNSResponder.reconfirm"))
(allow file-read*)
(allow file-write* (subpath "$work"))
(deny file-write* (subpath "$work/library"))
(deny network* (with send-signal SIGKILL))
EOF
  cat >"$work/canary.py" <<'PY'
import socket, sys
kind = sys.argv[1]
if kind == "external":
    sock = socket.socket()
    sock.settimeout(0.3)
    sock.connect(("203.0.113.5", 9))
elif kind == "dns":
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.sendto(b"\0", ("127.0.0.1", 53))
elif kind == "loopback-external":
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(0.3)
    sock.connect(("203.0.113.5", 9))
else:
    raise SystemExit(f"unknown canary {kind}")
PY
  sandbox_canary external
  sandbox_canary dns
  sandbox_canary loopback-external

  run_boxed() {
    local name="$1"
    shift
    if ! "${base_env[@]}" sandbox-exec -f "$work/sandbox.sb" \
      "$@" >"$work/$name.stdout" 2>"$work/$name.stderr"
    then
      echo "zero-network: $name failed inside the macOS sandbox" >&2
      cat "$work/$name.stderr" >&2
      exit 1
    fi
  }
  run_boxed scan "$albumen" scan "$work/library" --manifest "$work/out/scan.jsonl"
  grep -q 'scanned ' "$work/scan.stdout"
  run_boxed index "$mcp" --library "$work/library" <"$work/index.bin"
  check_mcp index "$work/index.stdout"
  run_boxed search "$mcp" --library "$work/library" <"$work/search.bin"
  check_mcp search "$work/search.stdout" "IMG_0001.jpg"
  assert_source_unchanged
  echo "zero-network: macOS sandbox-exec deny network* (SIGKILL) and mDNSResponder mach-lookup; scan, index, and search exited 0"
}

case "$(uname -s)" in
  Linux) run_linux ;;
  Darwin) run_macos ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "zero-network: on Windows run scripts/zero-network.ps1" >&2
    exit 1
    ;;
  *)
    echo "zero-network: unsupported operating system $(uname -s)" >&2
    exit 1
    ;;
esac
