#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Prove `albumen scan` and a receive dry-run open no sockets and do not write
# under the source. A host strace must not contain those syscalls. A rootless
# container with scripts/seccomp-no-network.json must still finish the scan,
# including the ExifTool child.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ "$(id -u)" -eq 0 ]]; then
  echo "scan-no-network: refusing to run as root" >&2
  exit 1
fi

runtime_dir="${XDG_RUNTIME_DIR:-/tmp/albumen-runtime-$(id -u)}"
mkdir -p "$runtime_dir"
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

engine=""
if command -v podman >/dev/null 2>&1; then
  rootless="$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null || true)"
  if [[ "$rootless" == "true" ]]; then
    engine="podman"
  fi
fi
if [[ -z "$engine" ]] && command -v docker >/dev/null 2>&1; then
  if docker info --format '{{.SecurityOptions}}' 2>/dev/null | grep -q rootless; then
    engine="docker"
  fi
fi
if [[ -z "$engine" ]]; then
  echo "scan-no-network: need rootless podman or rootless docker" >&2
  exit 1
fi
if ! command -v strace >/dev/null 2>&1; then
  echo "scan-no-network: need strace" >&2
  exit 1
fi

export PATH="$(scripts/install-exiftool.sh):$PATH"
exiftool_root="$(command -v exiftool)"
exiftool_root="$(cd "$(dirname "$exiftool_root")" && pwd)"
if [[ ! -x target/debug/albumen || ! -x target/debug/albumen-receive ]]; then
  cargo build --locked -p albumen -p albumen-receive
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/src/trip" "$work/out"
printf 'alpha' > "$work/src/a.txt"
printf 'jpeg' > "$work/src/trip/IMG.jpg"

strace -f -o "$work/scan.strace" \
  target/debug/albumen scan "$work/src" --manifest "$work/out/scan.jsonl"
target/debug/albumen verify-manifest --manifest "$work/out/scan.jsonl" --against "$work/src" >/dev/null
strace -f -o "$work/receive.strace" \
  target/debug/albumen-receive --to "$work/src" --user camera --password secret --webdav 127.0.0.1:9

python3 - "$work/scan.strace" "$work/receive.strace" "$work/src" <<'PY'
import sys
from pathlib import Path

source = str(Path(sys.argv[3]).resolve())
forbidden = (
    "socket(",
    "connect(",
    "bind(",
    "listen(",
    "accept(",
    "accept4(",
    "sendto(",
    "sendmsg(",
    "recvfrom(",
    "recvmsg(",
    "unlink(",
    "unlinkat(",
    "rename(",
    "renameat(",
    "renameat2(",
    "truncate(",
    "ftruncate(",
    "chmod(",
    "fchmod(",
    "fchmodat(",
    "setxattr(",
    "lsetxattr(",
    "fsetxattr(",
    "utimensat(",
)
write_flags = ("O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC")
for path in sys.argv[1:3]:
    for line in Path(path).read_text(errors="replace").splitlines():
        head = line.split("=", 1)[0]
        if any(name in head for name in forbidden):
            raise SystemExit(f"{path}: forbidden syscall: {line}")
        if "openat(" in head and source in line and any(flag in line for flag in write_flags):
            raise SystemExit(f"{path}: write open under source: {line}")
print("strace: no sockets and no source writes")
PY

if ! "$engine" image exists docker.io/library/ubuntu:24.04 >/dev/null 2>&1; then
  "$engine" pull docker.io/library/ubuntu:24.04
fi
"$engine" run --rm \
  --network none \
  --security-opt "seccomp=$root/scripts/seccomp-no-network.json" \
  -v "$work/src:/library:ro" \
  -v "$work/out:/out" \
  -v "$root/target/debug/albumen:/usr/local/bin/albumen:ro" \
  -v "$exiftool_root:/opt/exiftool:ro" \
  -e PATH=/opt/exiftool:/usr/local/bin:/usr/bin:/bin \
  docker.io/library/ubuntu:24.04 \
  /usr/local/bin/albumen scan /library --manifest /out/container.jsonl
"$engine" run --rm \
  --network none \
  --security-opt "seccomp=$root/scripts/seccomp-no-network.json" \
  -v "$work/src:/library:ro" \
  -v "$work/out:/out:ro" \
  -v "$root/target/debug/albumen:/usr/local/bin/albumen:ro" \
  -v "$exiftool_root:/opt/exiftool:ro" \
  -e PATH=/opt/exiftool:/usr/local/bin:/usr/bin:/bin \
  docker.io/library/ubuntu:24.04 \
  /usr/local/bin/albumen verify-manifest --manifest /out/container.jsonl --against /library

echo "scan-no-network: scan finished with sockets denied"
