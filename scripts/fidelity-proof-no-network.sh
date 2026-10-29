#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Prove `albumen takeout-fidelity` and `albumen verify-manifest --proof` open
# no network sockets and never write under the library, the Takeout, or the
# export they read. Each runs under strace -f, so the ExifTool child is
# included. Their only outputs (the fidelity file and the proof) go to a
# separate directory. Synthetic files only.
#
#   scripts/fidelity-proof-no-network.sh              run the audit (needs strace)
#   scripts/fidelity-proof-no-network.sh --self-test  check the strace reader only

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
check="$root/scripts/check-fidelity-proof-strace.py"

if [[ "${1:-}" == "--self-test" ]]; then
  exec python3 "$check" --self-test
fi
if ! command -v strace >/dev/null 2>&1; then
  echo "fidelity-proof-no-network: need strace" >&2
  exit 1
fi

export PATH="$(scripts/install-exiftool.sh):$PATH"
if [[ ! -x target/debug/albumen ]]; then
  cargo build --locked -p albumen
fi
bin="$root/target/debug/albumen"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export ALBUMEN_HOME="$work/home"
mkdir -p "$work/takeout" "$work/out"

python3 - "$root/fixtures/jpeg-gps/IMG_0001.jpg" "$work/takeout/takeout-001.zip" <<'PY'
import json, sys, zipfile
jpeg = open(sys.argv[1], "rb").read()
with zipfile.ZipFile(sys.argv[2], "w", zipfile.ZIP_STORED) as z:
    for name in ("A.jpg", "B.jpg"):
        info = zipfile.ZipInfo(f"Takeout/Google Photos/{name}", (2020, 5, 20, 16, 0, 0))
        z.writestr(info, jpeg)
        meta = {"title": name, "photoTakenTime": {"timestamp": "1590000000"}}
        info = zipfile.ZipInfo(f"Takeout/Google Photos/{name}.json", (2020, 5, 20, 16, 0, 0))
        z.writestr(info, json.dumps(meta))
PY

"$bin" import-takeout --apply --from "$work/takeout" --to "$work/library" --report "$work/out/report.jsonl" >/dev/null
"$bin" export --apply --from "$work/library" --to "$work/export" --manifest "$work/out/manifest.txt" >/dev/null

strace -f -o "$work/fidelity.strace" \
  "$bin" takeout-fidelity --report "$work/out/report.jsonl" --library "$work/library" --out "$work/out/fidelity.jsonl"
strace -f -o "$work/proof.strace" \
  "$bin" verify-manifest --manifest "$work/out/manifest.txt" --against "$work/export" --proof "$work/out/proof"
test -s "$work/out/fidelity.jsonl"
test -s "$work/out/proof.json"

python3 "$check" "$work/fidelity.strace" "$work/proof.strace" -- \
  "$work/takeout" "$work/library" "$work/export"
echo "fidelity-proof-no-network: no sockets, no writes under the library, Takeout, or export"
