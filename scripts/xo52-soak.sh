#!/usr/bin/env bash
# XO-52 real-device soak (#166). See docs/xo-52-soak-plan.md.
#
#   bash scripts/xo52-soak.sh day0     # set up copy A, copy B and the family libraries
#   bash scripts/xo52-soak.sh day N    # one soak day (N = 1, 2, ...)
#   bash scripts/xo52-soak.sh selftest # the whole plan on synthetic files in a temp dir
#
# Every write step runs as a dry run first. With SOAK_APPLY=1 it then runs
# with --apply. Without it, nothing is written except the logs and scan
# ledgers. Sources are only read.
set -euo pipefail

ALBUMEN="${ALBUMEN:-albumen}"

need() { [ -n "${!1:-}" ] || { echo "set $1 (see docs/xo-52-soak-plan.md)" >&2; exit 2; }; }

# run CMD...: the dry run, then (SOAK_APPLY=1) the same command with --apply.
run() {
  echo "+ $*"
  "$@"
  if [ "${SOAK_APPLY:-0}" = 1 ]; then
    echo "+ $* --apply"
    "$@" --apply
  fi
}

# kept OLD NEW: every file in scan manifest OLD is in NEW with the same
# path, size, mtime and SHA-256. NEW may have more files. The export's own
# ALBUMEN-MANIFEST.txt is rewritten by each export and is checked by
# verify-manifest instead.
kept() {
  python3 - "$1" "$2" <<'PY'
import json, sys
def load(p):
    out = {}
    for line in open(p):
        r = json.loads(line)
        if r["path"] == "ALBUMEN-MANIFEST.txt":
            continue
        out[r["path"]] = (r["bytes"], r["mtime_ns"], r["sha256"])
    return out
old, new = load(sys.argv[1]), load(sys.argv[2])
bad = [p for p, v in old.items() if new.get(p) != v]
for p in bad:
    print(f"CHANGED OR MISSING: {p}", file=sys.stderr)
print(f"kept {len(old) - len(bad)}/{len(old)} from {sys.argv[1]}; {len(new) - len(old)} new")
sys.exit(1 if bad else 0)
PY
}

vars() {
  need SOAK_SOURCE; need SOAK_HOME; need SOAK_B
  LOGS="$SOAK_HOME/logs"; A="$SOAK_HOME/A"; KEY="$SOAK_HOME/keys/soak.key"
  FAMILY_PARENT="$SOAK_HOME/family"
  mkdir -p "$LOGS" "$SOAK_HOME/keys"
}

family_a() { ls -d "$FAMILY_PARENT"/albumen-family/* 2>/dev/null | head -1; }
family_b() { echo "$SOAK_B/family/$(basename "$(family_a)")"; }

scan_all() {
  local tag="$1"
  "$ALBUMEN" scan "$SOAK_SOURCE" --manifest "$LOGS/source-$tag.jsonl"
  if [ -d "$A" ]; then "$ALBUMEN" scan "$A" --manifest "$LOGS/A-$tag.jsonl"; fi
  if [ -d "$SOAK_B/library" ]; then "$ALBUMEN" scan "$SOAK_B/library" --manifest "$LOGS/B-$tag.jsonl"; fi
  if [ -n "$(family_a)" ]; then
    "$ALBUMEN" scan "$(family_a)" --manifest "$LOGS/FA-$tag.jsonl"
    if [ -d "$(family_b)" ]; then "$ALBUMEN" scan "$(family_b)" --manifest "$LOGS/FB-$tag.jsonl"; fi
  fi
}

day0() {
  vars
  "$ALBUMEN" scan "$SOAK_SOURCE" --manifest "$LOGS/source-day0.jsonl"
  run "$ALBUMEN" import-icloud --from "$SOAK_SOURCE" --to "$A" --report "$LOGS/import-A.jsonl"
  # Copy A does not exist until the import is applied, so the later dry runs
  # (export, family) are shown on the SOAK_APPLY=1 pass.
  [ "${SOAK_APPLY:-0}" = 1 ] || { echo "dry run done; read the plan above, then rerun with SOAK_APPLY=1"; return; }
  run "$ALBUMEN" export --from "$A" --to "$SOAK_B/library"
  "$ALBUMEN" verify-manifest --manifest "$SOAK_B/library/ALBUMEN-MANIFEST.txt" --against "$SOAK_B/library"
  # The family libraries carry the signed hides and tombstones.
  [ -f "$KEY" ] || "$ALBUMEN" family keygen --key "$KEY" --apply
  [ -n "$(family_a)" ] || "$ALBUMEN" family init --to "$FAMILY_PARENT" --key "$KEY" --name soak --apply
  find "$A" -type f ! -name '*.json' ! -name '*.xmp' ! -name '*.aae' ! -name 'ALBUMEN-*' | sort | while read -r f; do
    "$ALBUMEN" family add --library "$(family_a)" --key "$KEY" --from "$f" --apply >/dev/null
  done
  run "$ALBUMEN" family sync --from "$(family_a)" --to "$(family_b)"
  scan_all day0
}

day() {
  vars
  local n="$1"
  # 1. New photos. SOAK_NEW is a folder of today's new photos (read-only).
  if [ -n "${SOAK_NEW:-}" ]; then
    run "$ALBUMEN" import-icloud --from "$SOAK_NEW" --to "$A" --report "$LOGS/import-A-day$n.jsonl"
    run "$ALBUMEN" export --from "$A" --to "$SOAK_B/library"
  fi
  # 2. Removals: SOAK_HIDE and SOAK_TOMBSTONE are SHA-256 lists, space-separated.
  for sha in ${SOAK_HIDE:-}; do
    run "$ALBUMEN" family hide --library "$(family_a)" --key "$KEY" --sha256 "$sha"
    echo "day$n hide $sha" >> "$LOGS/removals.txt"
  done
  for sha in ${SOAK_TOMBSTONE:-}; do
    run "$ALBUMEN" family tombstone --library "$(family_a)" --key "$KEY" --sha256 "$sha"
    echo "day$n tombstone $sha" >> "$LOGS/removals.txt"
  done
  # Receiver hide (Library::hide): no CLI or HTTP route exists; see the plan.
  # 3. Carry the signed records to B. It must print "removed: 0".
  run "$ALBUMEN" family sync --from "$(family_a)" --to "$(family_b)" | tee "$LOGS/sync-day$n.txt"
  if [ "${SOAK_APPLY:-0}" = 1 ]; then
    grep -q '^removed: 0$' "$LOGS/sync-day$n.txt"
    grep -q '^replaced: 0$' "$LOGS/sync-day$n.txt"
    # 4. Verify B and diff every copy against day 0.
    "$ALBUMEN" verify-manifest --manifest "$SOAK_B/library/ALBUMEN-MANIFEST.txt" --against "$SOAK_B/library"
  fi
  scan_all "day$n"
  local fail=0
  for c in source A B FA FB; do
    if [ -f "$LOGS/$c-day0.jsonl" ]; then kept "$LOGS/$c-day0.jsonl" "$LOGS/$c-day$n.jsonl" || fail=1; fi
  done
  [ "$fail" = 0 ] && echo "day $n: PASS" || { echo "day $n: FAIL; stop the soak and keep everything" >&2; exit 1; }
}

selftest() {
  local root
  root="$(mktemp -d)"
  export SOAK_SOURCE="$root/src" SOAK_HOME="$root/soak" SOAK_B="$root/raid/albumen-soak-B" SOAK_APPLY=1
  mkdir -p "$SOAK_SOURCE/2024" "$root/new"
  python3 - "$SOAK_SOURCE/2024" "$root/new" <<'PY'
import os, sys
for d, start in ((sys.argv[1], 0), (sys.argv[2], 10)):
    for i in range(start, start + 4):
        p = os.path.join(d, f"IMG_{i:04d}.JPG")
        open(p, "wb").write(b"\xff\xd8\xff\xe0" + os.urandom(4000) + b"\xff\xd9")
        os.utime(p, (1700000000 + i, 1700000000 + i))
PY
  chmod -R a-w "$SOAK_SOURCE"
  day0
  local s1 s2
  s1="$(sha256sum "$SOAK_SOURCE/2024/IMG_0001.JPG" | cut -d' ' -f1)"
  s2="$(sha256sum "$SOAK_SOURCE/2024/IMG_0002.JPG" | cut -d' ' -f1)"
  SOAK_NEW="$root/new" SOAK_HIDE="$s1" SOAK_TOMBSTONE="$s2" day 1
  chmod -R u+w "$root"
  rm -rf "$root"
  echo "selftest: PASS"
}

case "${1:-}" in
  day0) day0 ;;
  day) day "${2:?day number}" ;;
  selftest) selftest ;;
  *) sed -n '2,10p' "$0"; exit 2 ;;
esac
