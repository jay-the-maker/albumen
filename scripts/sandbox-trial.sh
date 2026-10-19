#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Rung 2 of docs/test-ladder.md.
# Copy a corpus, mount that copy read-only in a rootless container with no
# network, scan it, and require the copy's metadata to match afterwards.
# The original path is never mounted.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ "$(id -u)" -eq 0 ]]; then
  echo "sandbox-trial: refusing to run as root" >&2
  exit 1
fi

runtime_dir="${XDG_RUNTIME_DIR:-/tmp/albumen-runtime-$(id -u)}"
mkdir -p "$runtime_dir"
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

corpus="$root/fixtures"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --corpus)
      corpus="$2"
      shift 2
      ;;
    *)
      echo "usage: scripts/sandbox-trial.sh [--corpus DIR]" >&2
      exit 2
      ;;
  esac
done

engine=""
if command -v podman >/dev/null 2>&1; then
  rootless="$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null || true)"
  if [[ "$rootless" == "true" ]]; then
    engine="podman"
  else
    echo "sandbox-trial: podman is installed but it is not rootless" >&2
    exit 1
  fi
elif command -v docker >/dev/null 2>&1; then
  if docker info --format '{{.SecurityOptions}}' 2>/dev/null | grep -q rootless; then
    engine="docker"
  else
    echo "sandbox-trial: refusing rootful docker" >&2
    exit 1
  fi
else
  echo "sandbox-trial: need rootless podman or rootless docker" >&2
  exit 1
fi

export PATH="$(scripts/install-exiftool.sh):$PATH"
if [[ ! -x target/debug/albumen ]]; then
  cargo build --locked -p albumen
fi
exiftool_root="$(cd "$(dirname "$(command -v exiftool)")" && pwd)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/copy" "$work/out"
cp -a "$corpus/." "$work/copy/"

snapshot() {
  local dest="$1"
  python3 - "$work/copy" "$dest" <<'PY'
import os, sys
root, dest = sys.argv[1], sys.argv[2]
rows = []
for dirpath, dirnames, filenames in os.walk(root):
    dirnames.sort()
    for name in sorted(dirnames + filenames):
        path = os.path.join(dirpath, name)
        st = os.lstat(path)
        rel = os.path.relpath(path, root)
        kind = "dir" if os.path.isdir(path) and not os.path.islink(path) else "file"
        rows.append(f"{kind}\t{st.st_mtime_ns}\t{st.st_mode}\t{st.st_size}\t{rel}\n")
rows.sort()
with open(dest, "w", encoding="utf-8") as handle:
    handle.writelines(rows)
PY
}

snapshot "$work/before.txt"

if ! "$engine" image exists docker.io/library/ubuntu:24.04 >/dev/null 2>&1; then
  "$engine" pull docker.io/library/ubuntu:24.04
fi

"$engine" run --rm \
  --network none \
  --security-opt "seccomp=$root/scripts/seccomp-no-network.json" \
  -v "$work/copy:/library:ro" \
  --mount "type=tmpfs,destination=/out" \
  -v "$root/target/debug/albumen:/usr/local/bin/albumen:ro" \
  -v "$exiftool_root:/opt/exiftool:ro" \
  -e PATH=/opt/exiftool:/usr/local/bin:/usr/bin:/bin \
  docker.io/library/ubuntu:24.04 \
  /bin/sh -c '/usr/local/bin/albumen scan /library --manifest /out/scan.jsonl && /usr/local/bin/albumen verify-manifest --manifest /out/scan.jsonl --against /library'

snapshot "$work/after.txt"
if ! cmp -s "$work/before.txt" "$work/after.txt"; then
  echo "sandbox-trial: source copy metadata changed" >&2
  diff -u "$work/before.txt" "$work/after.txt" >&2 || true
  exit 1
fi

echo "rung 2 exit: source copy unchanged"
