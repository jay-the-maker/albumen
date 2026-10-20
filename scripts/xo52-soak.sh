#!/usr/bin/env bash
# XO-52 real-device soak (#166). See docs/xo-52-soak-plan.md.
#
#   bash scripts/xo52-soak.sh day0     # set up copy A, copy B, the family libraries and receiver library R
#   bash scripts/xo52-soak.sh day N    # one soak day (N = 1, 2, ...)
#   bash scripts/xo52-soak.sh selftest # the whole plan on synthetic files in a temp dir
#
# Every write step runs as a dry run first. With SOAK_APPLY=1 it then runs
# with --apply. Without it, nothing is written except the logs and scan
# ledgers, which go under ALBUMEN_HOME (default: the platform data directory),
# keyed by source. Nothing is written inside or beside a source.
set -euo pipefail

ALBUMEN="${ALBUMEN:-albumen}"
# Receiver library R is filled only from the repo's synthetic fixture corpus
# (fixtures/, CC0, checked against fixtures/SHA256SUMS), never from photos.
FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/fixtures"

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
  R="$SOAK_HOME/R"
  mkdir -p "$LOGS" "$SOAK_HOME/keys"
}

family_a() { ls -d "$FAMILY_PARENT"/albumen-family/* 2>/dev/null | head -1; }
family_b() { echo "$SOAK_B/family/$(basename "$(family_a)")"; }

scan_all() {
  local tag="$1"
  "$ALBUMEN" scan "$SOAK_SOURCE" --manifest "$LOGS/source-$tag.jsonl"
  if [ -d "$A" ]; then "$ALBUMEN" scan "$A" --manifest "$LOGS/A-$tag.jsonl"; fi
  if [ -d "$SOAK_B/library" ]; then "$ALBUMEN" scan "$SOAK_B/library" --manifest "$LOGS/B-$tag.jsonl"; fi
  if [ -d "$R" ]; then "$ALBUMEN" scan "$R" --manifest "$LOGS/R-$tag.jsonl"; fi
  if [ -n "$(family_a)" ]; then
    "$ALBUMEN" scan "$(family_a)" --manifest "$LOGS/FA-$tag.jsonl"
    if [ -d "$(family_b)" ]; then "$ALBUMEN" scan "$(family_b)" --manifest "$LOGS/FB-$tag.jsonl"; fi
  fi
}

# Fill receiver library R with the fixture corpus, flat (stored names are
# root-level, as the receiver stores them). Only a new, empty R is filled.
receiver_fill() {
  ( cd "$FIXTURES" && sha256sum --check --quiet SHA256SUMS ) \
    || { echo "fixtures/ does not match fixtures/SHA256SUMS; R is not filled" >&2; exit 1; }
  echo "+ fill $R from $FIXTURES ($(wc -l < "$FIXTURES/SHA256SUMS") synthetic files)"
  [ "${SOAK_APPLY:-0}" = 1 ] || return 0
  if [ -d "$R" ] && [ -n "$(ls -A "$R")" ]; then echo "R already filled; left as it is"; return 0; fi
  mkdir -p "$R"
  while read -r _ rel; do
    cp -p "$FIXTURES/$rel" "$R/$(basename "$rel")"
  done < "$FIXTURES/SHA256SUMS"
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
  receiver_fill
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
  # Receiver hide: SOAK_RECEIVER_HIDE is a list of stored names in R. A dry
  # run first; --apply only with SOAK_APPLY=1. Writes only the tombstone.
  for name in ${SOAK_RECEIVER_HIDE:-}; do
    run "$ALBUMEN" receive hide --library "$R" --name "$name"
    echo "day$n receiver-hide $name" >> "$LOGS/removals.txt"
    if [ "${SOAK_APPLY:-0}" = 1 ]; then
      grep -q '"hidden":true' "$R/$name.albumen-tombstone"
      [ -f "$R/$name" ] || { echo "receiver original $name is gone" >&2; exit 1; }
    fi
  done
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
  for c in source A B FA FB R; do
    if [ -f "$LOGS/$c-day0.jsonl" ]; then kept "$LOGS/$c-day0.jsonl" "$LOGS/$c-day$n.jsonl" || fail=1; fi
  done
  [ "$fail" = 0 ] && echo "day $n: PASS" || { echo "day $n: FAIL; stop the soak and keep everything" >&2; exit 1; }
}

selftest() {
  local root
  root="$(mktemp -d)"
  # Sources sit in $root/sources, so the soak folders are not in their parent.
  export ALBUMEN_HOME="$root/home"
  export SOAK_SOURCE="$root/sources/src" SOAK_HOME="$root/soak" SOAK_B="$root/raid/albumen-soak-B" SOAK_APPLY=1
  mkdir -p "$SOAK_SOURCE/2024" "$root/sources/new"
  python3 - "$SOAK_SOURCE/2024" "$root/sources/new" <<'PY'
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
  # A receiver-hide dry run writes nothing in R.
  local before after
  before="$(cd "$SOAK_HOME/R" && ls -la --time-style=full-iso | sha256sum)"
  "$ALBUMEN" receive hide --library "$SOAK_HOME/R" --name IMG_0001.jpg | grep -q '^dry-run'
  after="$(cd "$SOAK_HOME/R" && ls -la --time-style=full-iso | sha256sum)"
  [ "$before" = "$after" ] || { echo "selftest: a receive hide dry run wrote" >&2; exit 1; }
  SOAK_NEW="$root/sources/new" SOAK_HIDE="$s1" SOAK_TOMBSTONE="$s2" SOAK_RECEIVER_HIDE="IMG_0001.jpg" day 1
  # The receiver tombstone is on R only; the original is unchanged.
  if find "$SOAK_HOME/A" "$SOAK_B" -name '*.albumen-tombstone' | grep -q .; then
    echo "selftest: a receiver tombstone reached another copy" >&2; exit 1
  fi
  cmp "$SOAK_HOME/R/IMG_0001.jpg" "$FIXTURES/jpeg-gps/IMG_0001.jpg"
  # Nothing may appear beside the sources.
  if [ -e "$root/sources/src.ledger" ] || [ -e "$root/sources/new.ledger" ]; then
    echo "selftest: a ledger was written beside a source" >&2; exit 1
  fi
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
