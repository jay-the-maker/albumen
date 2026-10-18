#!/usr/bin/env bash
# Start a pinned MinIO server and run a command with ALBUMEN_S3_* set.
# MinIO is AGPL and is not part of Albumen. This downloads a released binary
# and runs it as a separate process. The credentials are disposable test values.
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: scripts/with-minio.sh cargo test --locked" >&2
  exit 2
fi

RELEASE="RELEASE.2025-09-07T16-13-09Z"
SHA256="7c5bd8512c6e966455b1d198209358b2d191c77a83ab377c4073281065fb855f"
URL="https://github.com/minio/minio/releases/download/${RELEASE}/minio.linux-amd64.${RELEASE}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
BIN="${CACHE}/minio-${RELEASE}"

mkdir -p "$CACHE"
if [[ ! -x "$BIN" ]] || ! echo "${SHA256}  ${BIN}" | sha256sum -c - >/dev/null 2>&1; then
  curl -fsSL -o "${BIN}.partial" "$URL"
  echo "${SHA256}  ${BIN}.partial" | sha256sum -c -
  chmod +x "${BIN}.partial"
  mv "${BIN}.partial" "$BIN"
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

set +e
"$@"
status=$?
set -e
if [[ $status -ne 0 ]]; then
  echo "Command failed. MinIO log:" >&2
  cat "$LOG" >&2
fi
exit "$status"
