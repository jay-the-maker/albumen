#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Day -1 synthetic dry run of the M1 soak scripts on repo fixtures/ only.
# Usage: dryrun.sh WORKDIR   (WORKDIR must not exist; nothing else is written)
# Builds a fake CORPUS from fixtures/ (day0 = all fixtures, daily batches =
# renamed fixture copies), then runs day0.sh and dayN.sh for days 1-5 with
# the Day 5 changed-copy fault on zz-synthetic/aae/IMG_0008.jpg.
# A day cannot be waited out here, so before each day the STALE folder's
# Day 0 proof time is moved back N days + 60 s (scratch copy only). That
# needs GNU date (Linux). Prints one PASS/FAIL line per day.
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
fx=$(cd "$here/../../fixtures" && pwd -P)
[ $# = 1 ] || { echo "usage: dryrun.sh WORKDIR" >&2; exit 2; }
[ ! -e "$1" ] || { echo "dryrun: $1 exists" >&2; exit 2; }
mkdir -p "$1"
w=$(cd "$1" && pwd -P)
export CORPUS="$w/corpus" LIB="$w/lib" DEST="$w/disk/dest" STALE="$w/disk/stale" SOAK="$w/soak"
export ALBUMEN_HOME="$w/home" FIXTURES="$fx"
mkdir -p "$CORPUS/day0" "$w/disk"
cp -Rp "$fx/." "$CORPUS/day0/"
for n in 01 02 03 04 05; do
  mkdir -p "$CORPUS/new/day$n"
  cp -p "$fx/aae/IMG_0008.jpg" "$CORPUS/new/day$n/d$n-IMG_A.jpg"
  cp -p "$fx/xmp/"*.xmp "$CORPUS/new/day$n/d$n-SIDE.xmp" 2>/dev/null || true
done
chmod -R a-w "$CORPUS"
before=$(cd "$CORPUS" && find . -type f -print0 | sort -z | xargs -0 sha256sum)

age_stale() { # N: pretend the STALE Day 0 backup was N days ago
  ts=$(($(date -u +%s) - $1 * 86400 - 60))
  t=$(date -u -d "@$ts" +%Y-%m-%dT%H:%M:%SZ)
  for j in "$STALE"/ALBUMEN-PROOFS/*.json; do
    sed -i "s/\"finished_utc\": \"[^\"]*\"/\"finished_utc\": \"$t\"/" "$j"
  done
}

result=0
sh "$here/day0.sh" || { echo "dryrun: day 0 failed, stopping" >&2; exit 1; }
for n in 1 2 3 4 5; do
  chmod u+w "$STALE/ALBUMEN-PROOFS" "$STALE"/ALBUMEN-PROOFS/*.json 2>/dev/null || true
  age_stale "$n"
  if [ "$n" = 5 ]; then
    sh "$here/dayN.sh" "$n" --fault aae/IMG_0008.jpg || result=1
  else
    sh "$here/dayN.sh" "$n" || result=1
  fi
done
after=$(cd "$CORPUS" && find . -type f -print0 | sort -z | xargs -0 sha256sum)
if [ "$before" = "$after" ]; then echo "corpus unchanged: ok"; else echo "corpus CHANGED" >&2; result=1; fi
chmod -R u+w "$CORPUS"
exit "$result"
