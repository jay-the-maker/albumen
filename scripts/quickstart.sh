#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Four user-facing steps from scripts/quickstart.json: install, receive,
# upload, verify. The source file is read and then checked again.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "--steps" ]]; then
  cat "$root/scripts/quickstart.json"
  exit 0
fi
if [[ -n "${1:-}" ]]; then
  echo "usage: quickstart.sh [--steps]" >&2
  exit 1
fi

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    os=windows
    export MSYS2_ARG_CONV_EXCL='*'
    export MSYS_NO_PATHCONV=1
    ;;
  *) os=unix ;;
esac

native() {
  if [[ "$os" == "windows" ]]; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

file_sha() {
  local path="$1"
  if [[ "$os" == "windows" ]]; then
    path="$(native "$path")"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print tolower($1)}'
  else
    shasum -a 256 "$path" | awk '{print tolower($1)}'
  fi
}

mtime_epoch() {
  if stat -c %Y "$1" >/dev/null 2>&1; then
    stat -c %Y "$1"
  else
    stat -f %m "$1"
  fi
}

: "${HOME:?}"
src_dir="$HOME/originals"
library="$HOME/albumen-library"
src="$src_dir/IMG_QUICKSTART.jpg"
mkdir -p "$src_dir"
printf 'albumen synthetic original. Not a photograph.\n' >"$src"
touch -t 202009131200.00 "$src"
hash_before="$(file_sha "$src")"
mtime_before="$(mtime_epoch "$src")"

# step: install
bash "$root/scripts/install.sh"
bindir="$HOME/.local/share/albumen/bin"
receive="$bindir/albumen-receive"
if [[ "$os" == "windows" ]]; then
  receive="$receive.exe"
fi
if [[ ! -x "$receive" ]]; then
  echo "installer did not place albumen-receive in $bindir" >&2
  exit 1
fi

# step: receive
export ALBUMEN_RECEIVE_PASSWORD="${ALBUMEN_RECEIVE_PASSWORD:-a receiver password}"
log="$(mktemp)"
receive_args=(
  --to "$(native "$library")"
  --user camera
  --webdav 127.0.0.1:8080
  --apply
)
if [[ "$os" == "windows" ]]; then
  "$(native "$receive")" "${receive_args[@]}" >"$log" 2>&1 &
else
  "$receive" "${receive_args[@]}" >"$log" 2>&1 &
fi
rpid=$!
cleanup() {
  if kill -0 "$rpid" 2>/dev/null; then
    kill "$rpid" 2>/dev/null || true
    wait "$rpid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if grep -q 'webdav http://' "$log"; then
    ready=1
    break
  fi
  if ! kill -0 "$rpid" 2>/dev/null; then
    cat "$log" >&2
    echo "receiver exited before it listened" >&2
    exit 1
  fi
  sleep 0.25
done
if [[ "$ready" != 1 ]]; then
  cat "$log" >&2
  echo "receiver did not listen on 127.0.0.1:8080" >&2
  exit 1
fi

# step: upload
mtime="$(mtime_epoch "$src")"
headers="$(mktemp)"
body="$(mktemp)"
curl -sS -D "$headers" -o "$body" -T "$(native "$src")" \
  -u "camera:${ALBUMEN_RECEIVE_PASSWORD}" \
  -H "Content-Type: application/octet-stream" \
  -H "X-Albumen-Mtime: ${mtime}" \
  "http://127.0.0.1:8080/IMG_QUICKSTART.jpg"

# step: verify
code="$(tr -d '\r' <"$headers" | awk '/^HTTP/ {status=$2} END {print status}')"
server="$(tr -d '\r' <"$headers" | awk 'tolower($1)=="x-albumen-sha256:" {print tolower($2)}')"
stored="$library/IMG_QUICKSTART.jpg"
if [[ ! -f "$stored" ]]; then
  echo "receiver did not store IMG_QUICKSTART.jpg" >&2
  cat "$headers" >&2
  exit 1
fi
client="$(file_sha "$src")"
stored_hash="$(file_sha "$stored")"
mtime_after="$(mtime_epoch "$src")"
if [[ "$code" != "201" || -z "$server" || "$server" != "$client" || "$stored_hash" != "$client" || "$client" != "$hash_before" ]]; then
  echo "SHA-256 verification failed" >&2
  echo "  status $code" >&2
  echo "  source $client" >&2
  echo "  header $server" >&2
  echo "  stored $stored_hash" >&2
  exit 1
fi
if [[ "$mtime_after" != "$mtime_before" ]]; then
  echo "source mtime changed" >&2
  exit 1
fi
echo "verified: $client"
echo "source unchanged"
