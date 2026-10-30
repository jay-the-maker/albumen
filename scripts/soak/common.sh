# SPDX-License-Identifier: Apache-2.0
# Shared helpers for the M1 soak scripts (protocol v1, sections 3-4).
# POSIX sh. Sourced by day0.sh, dayN.sh and audit.sh; not run directly.

# sha256 FILE...: shasum -a 256 on macOS, sha256sum on Linux. Both print
# "<hex>  <path>" and both accept -c.
case "$(uname -s)" in
Darwin) SHA256="shasum -a 256" ;;
*)
  if command -v sha256sum >/dev/null 2>&1; then
    SHA256="sha256sum"
  else
    SHA256="shasum -a 256"
  fi
  ;;
esac
export LC_ALL=C

die() {
  echo "soak: $*" >&2
  exit 2
}

need() {
  for v in "$@"; do
    eval "val=\${$v:-}"
    [ -n "$val" ] || die "set $v"
  done
}

# abs DIR: absolute path of an existing directory.
abs() { (cd "$1" && pwd -P); }

# inside A B: true when A is B or under B (both must exist).
inside() {
  a=$(abs "$1") b=$(abs "$2")
  case "$a/" in "$b"/*) return 0 ;; esac
  return 1
}

# Refuse any layout where an output folder sits in a source (CORPUS, LIB).
# The scripts only ever read CORPUS; LIB only gets the protocol's new files.
guard_layout() {
  for out in "$@"; do
    [ -d "$out" ] || continue
    for src in "${CORPUS:-}" "${LIB:-}"; do
      [ -n "$src" ] && [ -d "$src" ] || continue
      if inside "$out" "$src"; then die "$out is inside source $src; refusing"; fi
    done
    if [ -n "${CORPUS:-}" ] && [ -d "$CORPUS" ] && inside "$CORPUS" "$out"; then
      die "CORPUS is inside $out; refusing"
    fi
  done
}

# source_sha DIR OUT: the protocol's per-file listing of a source tree.
source_sha() {
  (cd "$1" && find . -type f -print0 | sort -z | xargs -0 $SHA256) >"$2"
}

# copy_new SRC_DIR DST_DIR: cp -p every file, never replacing one.
copy_new() {
  for f in "$1"/* "$1"/.[!.]*; do
    [ -e "$f" ] || continue
    [ ! -e "$2/${f##*/}" ] || die "$2/${f##*/} already exists; refusing to overwrite a source file"
    cp -Rp "$f" "$2/"
  done
}

ALBUMEN=${ALBUMEN:-albumen}
