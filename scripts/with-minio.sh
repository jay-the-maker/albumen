#!/usr/bin/env bash
# Start a pinned MinIO server and run a command with ALBUMEN_S3_* set.
# MinIO is AGPL and is not part of Albumen. This downloads a released binary
# and runs it as a separate process. The credentials are disposable test values.
#
# Docker is not used. Linux amd64, macOS arm64, and Windows amd64 have pinned
# binaries. Any other OS prints that and runs the command with
# ALBUMEN_S3_UNAVAILABLE=1 so the suite stays on the local filesystem.
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: scripts/with-minio.sh cargo test --locked" >&2
  exit 2
fi

check_sha256() {
  local sum="$1"
  local file="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    echo "${sum}  ${file}" | sha256sum -c - >&2
  elif command -v shasum >/dev/null 2>&1; then
    echo "${sum}  ${file}" | shasum -a 256 -c - >&2
  else
    echo "sha256sum or shasum is required" >&2
    exit 1
  fi
}

sha256_ok() {
  local sum="$1"
  local file="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    echo "${sum}  ${file}" | sha256sum -c - >/dev/null 2>&1
  else
    echo "${sum}  ${file}" | shasum -a 256 -c - >/dev/null 2>&1
  fi
}

RELEASE="RELEASE.2025-09-07T16-13-09Z"
OS_NAME="$(uname -s)"
ARCH_NAME="$(uname -m)"
case "${OS_NAME}-${ARCH_NAME}" in
  Linux-x86_64|Linux-amd64)
    ASSET="minio.linux-amd64.${RELEASE}"
    SHA256="7c5bd8512c6e966455b1d198209358b2d191c77a83ab377c4073281065fb855f"
    ;;
  Darwin-arm64)
    ASSET="minio.darwin-arm64.${RELEASE}"
    SHA256="7c3b3039b76e55a1b80935848ed83998d5e8d317374f87851f46a019ff5c0aa4"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    ASSET="minio.windows-amd64.${RELEASE}.exe"
    SHA256="af709e6ba68488404e85acdd22a3030d0f5e56a108d4b27d744f18ceb50861b4"
    ;;
  *)
    echo "Docker is not used. No pinned MinIO binary for ${OS_NAME}-${ARCH_NAME}. S3 will not run; the suite is local filesystem only." >&2
    export ALBUMEN_S3_UNAVAILABLE=1
    exec "$@"
    ;;
esac

URL="https://github.com/minio/minio/releases/download/${RELEASE}/${ASSET}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
BIN="${CACHE}/${ASSET}"

mkdir -p "$CACHE"
if [[ ! -x "$BIN" ]] || ! sha256_ok "$SHA256" "$BIN"; then
  curl -fsSL -o "${BIN}.partial" "$URL"
  check_sha256 "$SHA256" "${BIN}.partial"
  chmod +x "${BIN}.partial"
  mv "${BIN}.partial" "$BIN"
fi

if [[ "$OS_NAME" == "Darwin" ]]; then
  xattr -d com.apple.quarantine "$BIN" 2>/dev/null || true
fi

DATA="$(mktemp -d)"
LOG="$(mktemp)"
PORT="${ALBUMEN_MINIO_PORT:-9000}"

export ALBUMEN_S3_ACCESS_KEY="${ALBUMEN_S3_ACCESS_KEY:-albumen}"
export ALBUMEN_S3_SECRET_KEY="${ALBUMEN_S3_SECRET_KEY:-albumen-ci-not-a-secret}"
export MINIO_ROOT_USER="$ALBUMEN_S3_ACCESS_KEY"
export MINIO_ROOT_PASSWORD="$ALBUMEN_S3_SECRET_KEY"

"$BIN" server "$DATA" --address ":${PORT}" --console-address ":$((PORT + 1))" >"$LOG" 2>&1 &
PID=$!

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
  wait "$PID" >/dev/null 2>&1 || true
  rm -rf "$DATA"
}
trap cleanup EXIT

ready=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:${PORT}/minio/health/live" >/dev/null; then
    ready=1
    break
  fi
  if ! kill -0 "$PID" >/dev/null 2>&1; then
    echo "MinIO exited before it was healthy. Log:" >&2
    cat "$LOG" >&2
    exit 1
  fi
  sleep 0.5
done

if [[ "$ready" -ne 1 ]]; then
  echo "MinIO did not become healthy. Log:" >&2
  cat "$LOG" >&2
  exit 1
fi

export ALBUMEN_S3_ENDPOINT="${ALBUMEN_S3_ENDPOINT:-http://127.0.0.1:${PORT}}"
export ALBUMEN_S3_BUCKET="${ALBUMEN_S3_BUCKET:-albumen-ingest-test}"
export ALBUMEN_S3_REGION="${ALBUMEN_S3_REGION:-us-east-1}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RCLONE_DIR="$("${SCRIPT_DIR}/install-rclone.sh")"
export PATH="${RCLONE_DIR}:${PATH}"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) export ALBUMEN_RCLONE="${RCLONE_DIR}/rclone.exe" ;;
  *) export ALBUMEN_RCLONE="${RCLONE_DIR}/rclone" ;;
esac

set +e
"$@"
status=$?
set -e
if [[ $status -ne 0 ]]; then
  echo "Command failed. MinIO log:" >&2
  cat "$LOG" >&2
fi
exit "$status"
