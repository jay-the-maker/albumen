#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Independent audit of one backup folder (protocol v1 section 4).
# Usage: audit.sh DEST EVIDENCE_DIR [LIB]
# Does not run albumen. Reads DEST/ALBUMEN-MANIFEST.txt ("# albumen-manifest-v1",
# then "<sha256> <bytes> <path>" lines), re-hashes every file itself with
# shasum -a 256 / sha256sum, checks sizes, and lists missing and extra files.
# Writes audit.sha256, audit.diff, count.txt, audit.exit in EVIDENCE_DIR.
# Only reads DEST and LIB. Exit 0 only when audit.diff is empty.
set -eu
here=$(cd "$(dirname "$0")" && pwd -P)
. "$here/common.sh"
[ $# -ge 2 ] || die "usage: audit.sh DEST EVIDENCE_DIR [LIB]"
dest=$(abs "$1")
mkdir -p "$2"
e=$(abs "$2")
lib=${3:-${LIB:-}}
case "$e/" in "$dest"/*) die "evidence dir is inside DEST" ;; esac
w="$e/audit-work"
mkdir -p "$w"
man="$dest/ALBUMEN-MANIFEST.txt"
[ -f "$man" ] || die "no $man"
[ "$(head -n 1 "$man")" = "# albumen-manifest-v1" ] || die "unknown manifest header"

: >"$e/audit.sha256"
: >"$e/audit.diff"
: >"$w/man.list"
tail -n +2 "$man" | while IFS= read -r line; do
  [ -n "$line" ] || continue
  sha=${line%% *}
  rest=${line#* }
  bytes=${rest%% *}
  path=${rest#* }
  printf '%s  %s\n' "$sha" "$path" >>"$e/audit.sha256"
  printf '%s\n' "$path" >>"$w/man.list"
  if [ -f "$dest/$path" ]; then
    got=$(wc -c <"$dest/$path" | tr -d ' ')
    [ "$got" = "$bytes" ] || printf 'SIZE %s: manifest %s, disk %s\n' "$path" "$bytes" "$got" >>"$e/audit.diff"
  else
    printf 'MISSING %s\n' "$path" >>"$e/audit.diff"
  fi
done

# Hash check of every item that exists (missing ones are listed above).
awk 'FILENAME == ARGV[1] { if ($1 == "MISSING") { sub(/^MISSING /, ""); miss[$0] = 1 }; next }
     { p = substr($0, 67); if (!(p in miss)) print }' "$e/audit.diff" "$e/audit.sha256" >"$w/present.sha256"
(cd "$dest" && $SHA256 -c "$w/present.sha256" 2>/dev/null || true) |
  grep -v ': OK$' | sed 's/^/HASH /' >>"$e/audit.diff" || true

# Files on disk that the manifest does not list.
(cd "$dest" && find . -type f) | sed 's|^\./||' |
  grep -v -e '^ALBUMEN-MANIFEST\.txt$' -e '^ALBUMEN-PROOFS/' | sort >"$w/disk.list" || true
sort "$w/man.list" >"$w/man.sorted"
comm -13 "$w/man.sorted" "$w/disk.list" | sed 's/^/EXTRA /' >>"$e/audit.diff"

items=$(wc -l <"$w/man.sorted" | tr -d ' ')
disk=$(wc -l <"$w/disk.list" | tr -d ' ')
if [ -n "$lib" ]; then libn=$(find "$lib" -type f | wc -l | tr -d ' '); else libn="n/a"; fi
printf 'manifest items: %s\nfiles on disk: %s\nitems in LIB: %s\n' "$items" "$disk" "$libn" >"$e/count.txt"

if [ -s "$e/audit.diff" ]; then
  echo 1 >"$e/audit.exit"
  echo "audit: FAIL ($(wc -l <"$e/audit.diff" | tr -d ' ') problems, see $e/audit.diff)" >&2
  sed 's/^/  /' "$e/audit.diff" >&2
  exit 1
fi
echo 0 >"$e/audit.exit"
echo "audit: ok, $items items re-hashed"
