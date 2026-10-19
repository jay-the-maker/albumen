#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Exercise install.sh without a release binary: checksums, a rejected
# attestation, and a local download. A failed check must not copy binaries.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
installer="$root/scripts/install.sh"
artifact="$(bash "$installer" --print-artifact)"
bins=()
while IFS= read -r line; do
  bins+=("$line")
done < <(bash "$installer" --print-bins)

file_sha() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

write_sums() {
  local dir="$1"
  shift
  local out="$1"
  shift
  : >"$out"
  local file
  for file in "$@"; do
    printf '%s  %s\n' "$(file_sha "$dir/$file")" "$file" >>"$out"
  done
}

make_archive() {
  local dest="$1"
  local mode="${2:-good}"
  local stage
  stage="$(mktemp -d)"
  local bin
  for bin in "${bins[@]}"; do
    printf 'albumen-bin %s\n' "$bin" >"$stage/$bin"
  done
  if [[ "$mode" == "bad-inner" ]]; then
    printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' "${bins[0]}" >"$stage/SHA256SUMS"
    local i=1
    while [[ "$i" -lt ${#bins[@]} ]]; do
      bin="${bins[$i]}"
      printf '%s  %s\n' "$(file_sha "$stage/$bin")" "$bin" >>"$stage/SHA256SUMS"
      i=$((i + 1))
    done
  else
    write_sums "$stage" "$stage/SHA256SUMS" "${bins[@]}"
  fi
  mkdir -p "$dest"
  (
    cd "$stage"
    tar -czf "$dest/${artifact}.tar.gz" SHA256SUMS "${bins[@]}"
  )
  printf '%s  %s\n' "$(file_sha "$dest/${artifact}.tar.gz")" "${artifact}.tar.gz" >"$dest/${artifact}.tar.gz.sha256"
  rm -rf "$stage"
}

assert_absent() {
  if [[ -e "$1" ]]; then
    echo "installer wrote $1 after a failed check" >&2
    exit 1
  fi
}

run_fail() {
  local home="$1"
  shift
  local status=0
  mkdir -p "$home"
  set +e
  "$@" >"$home/install.out" 2>&1
  status=$?
  set -e
  if [[ "$status" -eq 0 ]]; then
    echo "installer succeeded and should have failed" >&2
    cat "$home/install.out" >&2
    exit 1
  fi
  assert_absent "$home/.local/share/albumen/bin/${bins[0]}"
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

good="$work/good"
make_archive "$good" good
home="$work/home-good"
mkdir -p "$home"
out="$(HOME="$home" ALBUMEN_ARCHIVE="$good/${artifact}.tar.gz" bash "$installer")"
printf '%s\n' "$out" | grep -q 'attestation: not available; SHA-256 was checked'
printf '%s\n' "$out" | grep -q "installed: $home/.local/share/albumen/bin"
grep -q "albumen-bin ${bins[0]}" "$home/.local/share/albumen/bin/${bins[0]}"

bad="$work/bad"
make_archive "$bad" good
sum_line="$(cat "$bad/${artifact}.tar.gz.sha256")"
printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' "${artifact}.tar.gz" >"$bad/${artifact}.tar.gz.sha256"
run_fail "$work/home-bad" env HOME="$work/home-bad" ALBUMEN_ARCHIVE="$bad/${artifact}.tar.gz" bash "$installer"
printf '%s\n' "$sum_line" >/dev/null

inner="$work/inner"
make_archive "$inner" bad-inner
run_fail "$work/home-inner" env HOME="$work/home-inner" ALBUMEN_ARCHIVE="$inner/${artifact}.tar.gz" bash "$installer"
grep -q 'SHA-256 mismatch' "$work/home-inner/install.out"

fakebin="$work/fakebin"
mkdir -p "$fakebin"
cat >"$fakebin/cosign" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"${COSIGN_LOG:?}"
exit "${COSIGN_RC:-0}"
EOF
cat >"$fakebin/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"${GH_LOG:?}"
if [ "${GH_RC:-0}" -ne 0 ]; then
  printf '%s\n' "${GH_ERR:-verification failed}" >&2
  exit "${GH_RC}"
fi
exit 0
EOF
chmod 755 "$fakebin/cosign" "$fakebin/gh"

cosign_reject="$work/cosign-reject"
make_archive "$cosign_reject" good
: >"$cosign_reject/${artifact}.${bins[0]}.bundle"
log="$work/cosign-reject.log"
run_fail "$work/home-cosign-reject" env \
  HOME="$work/home-cosign-reject" \
  PATH="$fakebin:$PATH" \
  COSIGN_LOG="$log" \
  COSIGN_RC=1 \
  ALBUMEN_ARCHIVE="$cosign_reject/${artifact}.tar.gz" \
  bash "$installer"
grep -q 'verify-blob' "$log"
grep -q -- '--bundle' "$log"

cosign_ok="$work/cosign-ok"
make_archive "$cosign_ok" good
: >"$cosign_ok/${artifact}.${bins[0]}.bundle"
log="$work/cosign-ok.log"
home="$work/home-cosign-ok"
mkdir -p "$home"
out="$(env HOME="$home" PATH="$fakebin:$PATH" COSIGN_LOG="$log" COSIGN_RC=0 \
  ALBUMEN_ARCHIVE="$cosign_ok/${artifact}.tar.gz" bash "$installer")"
printf '%s\n' "$out" | grep -q 'attestation: cosign verified'
grep -q "albumen-bin ${bins[0]}" "$home/.local/share/albumen/bin/${bins[0]}"

gh_reject_log="$work/gh-reject.log"
home="$work/home-gh-reject"
run_fail "$home" env \
  HOME="$home" \
  PATH="$fakebin:$PATH" \
  GH_LOG="$gh_reject_log" \
  GH_RC=1 \
  GH_ERR='verification failed' \
  ALBUMEN_ATTEST_WITH_GH=1 \
  ALBUMEN_ARCHIVE="$good/${artifact}.tar.gz" \
  bash "$installer"
grep -q 'attestation verify' "$gh_reject_log"

gh_miss_log="$work/gh-miss.log"
home="$work/home-gh-miss"
mkdir -p "$home"
out="$(env HOME="$home" PATH="$fakebin:$PATH" GH_LOG="$gh_miss_log" GH_RC=1 \
  GH_ERR='no matching attestations' \
  ALBUMEN_ATTEST_WITH_GH=1 \
  ALBUMEN_ARCHIVE="$good/${artifact}.tar.gz" bash "$installer")"
printf '%s\n' "$out" | grep -q 'attestation: gh could not find one; SHA-256 was checked'

gh_ok_log="$work/gh-ok.log"
home="$work/home-gh-ok"
mkdir -p "$home"
out="$(env HOME="$home" PATH="$fakebin:$PATH" GH_LOG="$gh_ok_log" GH_RC=0 \
  ALBUMEN_ATTEST_WITH_GH=1 \
  ALBUMEN_ARCHIVE="$good/${artifact}.tar.gz" bash "$installer")"
printf '%s\n' "$out" | grep -q 'attestation: gh verified'

outside="$work/home-outside"
mkdir -p "$outside"
run_fail "$outside" env \
  HOME="$outside" \
  ALBUMEN_HOME="/tmp/albumen-install-outside-$$" \
  ALBUMEN_ARCHIVE="$good/${artifact}.tar.gz" \
  bash "$installer"
if [[ -d "/tmp/albumen-install-outside-$$" ]]; then
  echo "installer created a directory outside the home directory" >&2
  rm -rf "/tmp/albumen-install-outside-$$"
  exit 1
fi

py=python3
if ! command -v python3 >/dev/null 2>&1; then
  py=python
fi
port="$("$py" -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
srv="$work/srv"
mkdir -p "$srv/latest/download"
cp "$good/${artifact}.tar.gz" "$good/${artifact}.tar.gz.sha256" "$srv/latest/download/"
"$py" -m http.server "$port" --bind 127.0.0.1 --directory "$srv" >"$work/http.log" 2>&1 &
http_pid=$!
cleanup_http() {
  kill "$http_pid" 2>/dev/null || true
  wait "$http_pid" 2>/dev/null || true
}
trap 'cleanup_http; rm -rf "$work"' EXIT
ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if curl -fsS "http://127.0.0.1:${port}/latest/download/${artifact}.tar.gz.sha256" -o /dev/null 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.1
done
if [[ "$ready" != 1 ]]; then
  cat "$work/http.log" >&2
  echo "local release server did not start" >&2
  exit 1
fi
home="$work/home-http"
mkdir -p "$home"
out="$(env -u ALBUMEN_ARCHIVE HOME="$home" ALBUMEN_RELEASE_BASE="http://127.0.0.1:${port}" bash "$installer")"
printf '%s\n' "$out" | grep -q "installed: $home/.local/share/albumen/bin"
grep -q "albumen-bin ${bins[0]}" "$home/.local/share/albumen/bin/${bins[0]}"
echo "install.sh checks: ok"
