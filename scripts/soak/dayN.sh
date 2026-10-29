#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# M1 soak Day N (protocol v1 section 4). Usage: dayN.sh N [--fault RELPATH]
# Env: CORPUS LIB DEST STALE SOAK, FIXTURES (default: repo fixtures/),
# ALBUMEN (default: albumen), STALE_FROM (default 3: first day STALE must
# be stale under --max-age 2d).
# --fault RELPATH (Day 5): after the backup, flip one byte of
# DEST/zz-synthetic/RELPATH, expect verify, status and the audit to catch
# exactly that file, then restore it from FIXTURES with cp -p and expect a
# clean re-verify and re-audit.
# CORPUS and FIXTURES are only read; LIB only gets that day's new files.
# Exit 0 only when every exit code and check is what the protocol expects.
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
. "$here/common.sh"
FIXTURES=${FIXTURES:-$here/../../fixtures}
STALE_FROM=${STALE_FROM:-3}
need CORPUS LIB DEST STALE SOAK
[ $# -ge 1 ] || die "usage: dayN.sh N [--fault RELPATH]"
N=$(printf '%02d' "$1")
fault=""
if [ "${2:-}" = "--fault" ]; then fault=${3:?--fault needs a path under zz-synthetic}; fi
guard_layout "$DEST" "$STALE" "$SOAK"
E="$SOAK/day$N"
mkdir "$E"
echo "day $N:"
fails=0
check() { # name expected actual
  if [ "$2" = "$3" ]; then echo "  $1: exit $3 (expected $2) ok"; else
    echo "  $1: exit $3, expected $2 FAIL"
    fails=$((fails + 1))
  fi
}
run_exit() { # outfile errfile cmd...: run, keep output, print exit code
  o=$1 er=$2
  shift 2
  set +e
  if [ "$o" = "$er" ]; then "$@" >"$o" 2>&1; else "$@" >"$o" 2>"$er"; fi
  rc=$?
  set -e
  echo "$rc"
}

# New photos for the day (none is fine; it is logged).
if [ -d "$CORPUS/new/day$N" ]; then
  copy_new "$CORPUS/new/day$N" "$LIB"
  ls "$CORPUS/new/day$N" >>"$SOAK/adds.txt"
fi

rc=$(run_exit "$E/backup.out" "$E/backup.err" "$ALBUMEN" backup --from "$LIB" --to "$DEST" --state "$SOAK/state")
echo "$rc" >"$E/backup.exit"
check backup 0 "$rc"

bad=0
if [ -n "$fault" ]; then
  t="$DEST/zz-synthetic/$fault"
  [ -f "$t" ] || die "no $t"
  old=$($SHA256 "$t" | cut -d' ' -f1)
  # Flip the first byte (XOR 0xff) in place.
  first=$(od -An -tu1 -N1 "$t" | tr -d ' ')
  flipped=$(((first ^ 255) & 255))
  {
    printf "\\$(printf '%03o' "$flipped")"
    tail -c +2 "$t"
  } >"$E/fault-copy.bin"
  cat "$E/fault-copy.bin" >"$t"
  new=$($SHA256 "$t" | cut -d' ' -f1)
  printf 'day%s changed-copy zz-synthetic/%s old %s new %s\n' "$N" "$fault" "$old" "$new" >>"$SOAK/faults.txt"
  [ "$old" != "$new" ] || die "fault did not change the file"
  bad=1
fi

rc=$(run_exit "$E/status.out" "$E/status.out" "$ALBUMEN" status "$DEST")
echo "$rc" >"$E/status.exit"
check status "$bad" "$rc"

rc=$(run_exit "$E/stale.out" "$E/stale.out" "$ALBUMEN" status --max-age 2d "$STALE")
echo "$rc" >"$E/stale.exit"
if [ "$(expr "$N" + 0)" -ge "$STALE_FROM" ]; then want=1; else want=0; fi
check "stale (--max-age 2d)" "$want" "$rc"
if [ "$rc" = 0 ] && [ "$want" = 1 ]; then echo "  STALE reported ok after day $STALE_FROM: lying status"; fi

rc=$(run_exit "$E/verify.out" "$E/verify.out" "$ALBUMEN" verify-manifest --manifest "$DEST/ALBUMEN-MANIFEST.txt" --against "$DEST" --proof "$E/verify")
echo "$rc" >"$E/verify.exit"
check verify-manifest "$bad" "$rc"

set +e
"$here/audit.sh" "$DEST" "$E" "$LIB" >"$E/audit.out" 2>&1
rc=$?
set -e
check audit "$bad" "$rc"
if [ -n "$fault" ]; then
  if [ "$(cat "$E/audit.diff")" = "HASH zz-synthetic/$fault: FAILED" ]; then
    echo "  audit.diff names only zz-synthetic/$fault ok"
  else
    echo "  audit.diff does not name exactly the fault file FAIL"
    fails=$((fails + 1))
  fi
  grep -q "zz-synthetic/$fault" "$E/verify.out" || {
    echo "  verify output does not name the fault file FAIL"
    fails=$((fails + 1))
  }
  # Repair: copy the fixture back, then everything must be clean again.
  cp -p "$FIXTURES/$fault" "$t"
  echo "day$N repaired zz-synthetic/$fault from fixtures" >>"$SOAK/faults.txt"
  rc=$(run_exit "$E/verify-repaired.out" "$E/verify-repaired.out" "$ALBUMEN" verify-manifest --manifest "$DEST/ALBUMEN-MANIFEST.txt" --against "$DEST" --proof "$E/verify-repaired")
  check "verify after repair" 0 "$rc"
  set +e
  "$here/audit.sh" "$DEST" "$E/repaired" "$LIB" >"$E/audit-repaired.out" 2>&1
  rc=$?
  set -e
  check "audit after repair" 0 "$rc"
fi

# The source may only have gained the day's new files.
source_sha "$LIB" "$E/source.sha"
diff "$SOAK/source-day00.sha" "$E/source.sha" >"$E/source.diff" || true
if grep '^<' "$E/source.diff" >/dev/null; then
  echo "  source.diff: a Day 0 file changed or vanished FAIL (STOP)"
  fails=$((fails + 1))
else
  unknown=0
  grep '^>' "$E/source.diff" | sed 's|^> [0-9a-f]*  \./||' | while IFS= read -r p; do
    grep -Fqx "$p" "$SOAK/adds.txt" || { echo "  source.diff: $p is not in adds.txt"; exit 1; }
  done || unknown=1
  if [ "$unknown" = 0 ]; then echo "  source.diff: only adds.txt files ok"; else
    echo "  source.diff FAIL (STOP)"
    fails=$((fails + 1))
  fi
fi

if [ "$fails" = 0 ]; then echo "day $N: PASS"; else
  echo "day $N: FAIL ($fails checks); STOP the soak and freeze SOAK, LIB and DEST" >&2
  exit 1
fi
