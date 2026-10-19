#!/usr/bin/env bash
# Prove albumen, albumen-mcp, and albumen-receive open a network connection
# only to a destination the invocation named.
#
# The processes run in a fresh user network namespace. That namespace has
# loopback and no route anywhere else, so an unexpected connect cannot leave
# the machine. strace logs connect, sendto, sendmsg, and bind. scripts/check-egress.py
# fails the run when a logged address is not on the command's allow-list.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "no-unexpected-egress: this proof runs on Linux" >&2
  exit 1
fi
if [[ "$(id -u)" -eq 0 ]]; then
  echo "no-unexpected-egress: refusing to run as root" >&2
  exit 1
fi
if ! command -v strace >/dev/null 2>&1; then
  echo "no-unexpected-egress: need strace" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "no-unexpected-egress: need python3" >&2
  exit 1
fi
python3 "$root/scripts/check-egress.py" --self-test
python3 "$root/scripts/check-ledger-egress.py" --self-test

if ! unshare --user --map-root-user --net true; then
  echo "no-unexpected-egress: unshare --net failed. On Ubuntu, set kernel.apparmor_restrict_unprivileged_userns=0 and retry." >&2
  exit 1
fi

export PATH="$(scripts/install-exiftool.sh):$PATH"
cargo build --locked -p albumen -p albumen-mcp -p albumen-receive

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src" "$work/library" "$work/destination" "$work/out" "$work/trace" "$work/logs" "$work/status"
printf 'jpeg-bytes' > "$work/src/IMG.jpg"
cp "$work/src/IMG.jpg" "$work/library/IMG.jpg"

python3 - "$work/mcp-dry.bin" "$work/out/mcp-export" "$work/src/IMG.jpg" <<'PY'
import json
import sys

def frame(message):
    body = json.dumps(message).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body

out, manifest, upload = sys.argv[1:]
messages = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "search", "arguments": {"query": "IMG"}}},
    {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "export", "arguments": {"to": "s3://library/photos", "manifest": manifest}}},
    {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "upload", "arguments": {"path": upload}}},
]
open(out, "wb").write(b"".join(frame(message) for message in messages))
PY
python3 - "$work/mcp-s3.bin" "$work/out/mcp-s3-manifest.txt" <<'PY'
import json
import sys

def frame(message):
    body = json.dumps(message).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body

out, manifest = sys.argv[1:]
messages = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "export", "arguments": {"to": "s3://library/photos", "manifest": manifest}}},
]
open(out, "wb").write(b"".join(frame(message) for message in messages))
PY

cat > "$work/inside.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import fcntl
import socket
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
ifr = bytearray(40)
ifr[:2] = b"lo"
res = fcntl.ioctl(sock.fileno(), 0x8913, bytes(ifr))
flags = int.from_bytes(res[16:18], "little") | 0x1
buf = bytearray(res)
buf[16:18] = flags.to_bytes(2, "little")
fcntl.ioctl(sock.fileno(), 0x8914, bytes(buf))
PY

python3 - "$work/s3-ready" <<'PY' &
import os
import socket
import sys
import time
ready = sys.argv[1]
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", 19000))
server.listen(32)
server.settimeout(1)
open(ready, "w").close()
deadline = time.time() + 90
while time.time() < deadline:
    try:
        client, _addr = server.accept()
    except socket.timeout:
        continue
    try:
        client.settimeout(1)
        client.recv(65536)
        client.sendall(b"HTTP/1.1 500 no\\r\\nContent-Length: 2\\r\\nConnection: close\\r\\n\\r\\nno")
    except OSError:
        pass
    finally:
        client.close()
PY
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [[ -f "$work/s3-ready" ]] && break
  sleep 0.1
done
[[ -f "$work/s3-ready" ]]

run_case() {
  local name="\$1" allow_connect="\$2" allow_bind="\$3" expect="\$4"
  shift 4
  set +e
  strace -f -yy -s 256 -e trace=connect,sendto,sendmsg,bind,listen \\
    -o "$work/trace/\${name}.strace" \\
    "\$@" >"$work/logs/\${name}" 2>&1
  local status=\$?
  set -e
  echo "\$status" > "$work/status/\${name}"
  python3 "$root/scripts/check-egress.py" \\
    "$work/trace/\${name}.strace" "\$allow_connect" "\$allow_bind" "\$name"
  if [[ "\$status" != "\$expect" ]]; then
    if [[ "\$expect" == "124" && "\$status" == "137" ]]; then
      :
    else
      echo "\$name exited \$status, expected \$expect" >&2
      cat "$work/logs/\${name}" >&2
      exit 1
    fi
  fi
}

base_env=(env -i PATH="$PATH" HOME="$HOME" LANG=C.UTF-8 TMPDIR="$work" ALBUMEN_CONNECTION_LOG="$work/connections.jsonl")
albumen="$root/target/debug/albumen"
mcp="$root/target/debug/albumen-mcp"
receive="$root/target/debug/albumen-receive"

run_case scan "" "" 0 \\
  "\${base_env[@]}" "\$albumen" scan "$work/src" --manifest "$work/out/scan.jsonl"
run_case export-local "" "" 0 \\
  "\${base_env[@]}" "\$albumen" export --from "$work/src" --to "$work/out/export" --apply
run_case export-s3-plan "" "" 0 \\
  "\${base_env[@]}" "\$albumen" export --from "$work/src" --to s3://library/photos --manifest "$work/out/s3-plan.txt"
run_case import-takeout "" "" 0 \\
  "\${base_env[@]}" "\$albumen" import-takeout --from "$work/src" --to "$work/out/takeout" --report "$work/out/takeout.jsonl"
run_case import-icloud "" "" 0 \\
  "\${base_env[@]}" "\$albumen" import-icloud --from "$work/src" --to "$work/out/icloud" --report "$work/out/icloud.jsonl"
run_case platform-dirs "" "" 0 \\
  "\${base_env[@]}" "\$albumen" platform-dirs
run_case mcp-dry "" "" 0 \\
  "\${base_env[@]}" "\$mcp" --library "$work/library" --destination "$work/destination" --enable-export --enable-upload \\
  < "$work/mcp-dry.bin"
run_case receive-plan "" "" 0 \\
  "\${base_env[@]}" "\$receive" --to "$work/out/camera" --user camera --webdav 127.0.0.1:18080
run_case export-s3 "127.0.0.1:19000" "" 1 \\
  "\${base_env[@]}" \\
  ALBUMEN_S3_ENDPOINT=http://127.0.0.1:19000 \\
  ALBUMEN_S3_BUCKET=library \\
  ALBUMEN_S3_ACCESS_KEY=albumen \\
  ALBUMEN_S3_SECRET_KEY=albumen-ci-not-a-secret \\
  "\$albumen" export --from "$work/src" --to s3://library/photos --manifest "$work/out/s3-manifest.txt" --apply
run_case mcp-s3 "127.0.0.1:19000" "" 0 \\
  "\${base_env[@]}" \\
  ALBUMEN_S3_ENDPOINT=http://127.0.0.1:19000 \\
  ALBUMEN_S3_BUCKET=library \\
  ALBUMEN_S3_ACCESS_KEY=albumen \\
  ALBUMEN_S3_SECRET_KEY=albumen-ci-not-a-secret \\
  "\$mcp" --library "$work/library" --destination "$work/destination" --enable-export --apply \\
  < "$work/mcp-s3.bin"
run_case receive-listen "" "18080" 124 \\
  "\${base_env[@]}" ALBUMEN_RECEIVE_PASSWORD=receiver-secret \\
  timeout --signal=TERM --kill-after=2 3 \\
  "\$receive" --to "$work/out/listen" --user camera --webdav 127.0.0.1:18080 --apply
run_case ledger "" "" 0 \\
  "\${base_env[@]}" "\$albumen" ledger --library "$work/library" --connection-log "$work/connections.jsonl" --json
python3 "$root/scripts/check-ledger-egress.py" "$work/trace" "$work/logs/ledger"

echo "no-unexpected-egress: each binary stayed on the destinations it was given"
EOF

unshare --user --map-root-user --net bash "$work/inside.sh"
echo "no-unexpected-egress: namespace had no route beyond loopback"
