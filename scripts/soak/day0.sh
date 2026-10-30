#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# M1 soak Day 0 (protocol v2 section 4). Env: CORPUS LIB DEST STALE SOAK
# FIXTURES (default: this repo's fixtures/), ALBUMEN (default: albumen).
# CORPUS and FIXTURES are only read. LIB must not exist yet.
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
. "$here/common.sh"
FIXTURES=${FIXTURES:-$here/../../fixtures}
need CORPUS LIB DEST STALE SOAK
[ -d "$CORPUS/day0" ] || die "no $CORPUS/day0"
[ ! -e "$LIB" ] || die "LIB $LIB exists; Day 0 starts from an empty copy"
for d in "$DEST" "$STALE"; do
  [ ! -e "$d" ] || [ -z "$(ls -A "$d")" ] || die "$d is not empty"
done
mkdir -p "$LIB" "$DEST" "$STALE" "$SOAK"
guard_layout "$DEST" "$STALE" "$SOAK"

(cd "$FIXTURES" && $SHA256 -c SHA256SUMS >/dev/null) || die "fixtures/SHA256SUMS does not match"
cp -Rp "$CORPUS/day0/." "$LIB/"
# cp -p keeps a read-only CORPUS's modes; LIB folders must take new files.
find "$LIB" -type d -exec chmod u+w {} +
mkdir "$LIB/zz-synthetic"
cp -Rp "$FIXTURES/." "$LIB/zz-synthetic/"
source_sha "$LIB" "$SOAK/source-day00.sha"
: >"$SOAK/adds.txt"
: >"$SOAK/faults.txt"

fail=0
"$ALBUMEN" backup --from "$LIB" --to "$DEST" --state "$SOAK/state" >"$SOAK/day00-backup.out" 2>&1 || fail=1
"$ALBUMEN" backup --from "$LIB" --to "$STALE" --state "$SOAK/state-stale" >"$SOAK/day00-stale.out" 2>&1 || fail=1
{
  # There is no albumen --version (protocol v2 erratum 4): record the build.
  printf 'git HEAD: %s\n' "$(git -C "$here" rev-parse HEAD 2>/dev/null || echo unknown)"
  for j in "$DEST"/ALBUMEN-PROOFS/*.json; do
    if [ -f "$j" ]; then sed -n 's/.*"commit": *"\([^"]*\)".*/proof commit: \1/p' "$j"; fi
  done
  uname -a
  if command -v sw_vers >/dev/null 2>&1; then sw_vers; sysctl -n hw.model; fi
  printf 'LIB files: %s\n' "$(wc -l <"$SOAK/source-day00.sha" | tr -d ' ')"
} >"$SOAK/day00-env.txt"
if [ "$fail" = 0 ]; then echo "day 00: PASS"; else
  echo "day 00: FAIL (see $SOAK/day00-*.out)" >&2
  exit 1
fi
