#!/usr/bin/env bash
# Make ffmpeg available for the synthetic Live Photo generator.
# The bytes that generator produces are hashed in the same run, so this does
# not pin a binary. ExifTool stays the pinned metadata reader.
set -euo pipefail

has_encoder() {
  command -v ffmpeg >/dev/null 2>&1 && ffmpeg -hide_banner -encoders 2>/dev/null | grep -q libx265
}

if has_encoder; then
  exit 0
fi

os="$(uname -s)"
case "$os" in
  Linux)
    sudo apt-get update
    sudo apt-get install -y ffmpeg
    ;;
  Darwin)
    brew install ffmpeg
    ;;
  MINGW*|MSYS*|CYGWIN*)
    # Chocolatey can exit 0 when the community feed returns 503 and no
    # ffmpeg.exe was written. Retry until the binary is actually there.
    bin=""
    attempt=1
    while [[ "$attempt" -le 5 ]]; do
      choco install ffmpeg -y --no-progress || true
      for candidate in \
        /c/ProgramData/chocolatey/lib/ffmpeg/tools/ffmpeg/bin \
        /c/ProgramData/chocolatey/bin
      do
        if [[ -x "$candidate/ffmpeg.exe" || -x "$candidate/ffmpeg" ]]; then
          bin="$candidate"
          break
        fi
      done
      if [[ -n "$bin" ]]; then
        break
      fi
      echo "ffmpeg install did not produce ffmpeg.exe (attempt ${attempt})" >&2
      if [[ "$attempt" -eq 5 ]]; then
        exit 1
      fi
      sleep $((attempt * 10))
      attempt=$((attempt + 1))
    done
    # The Chocolatey shim is not a reliable PATH entry for the next step.
    # Publish the directory that contains ffmpeg.exe as a Windows path.
    export PATH="${bin}:${PATH}"
    if [[ -n "${GITHUB_PATH:-}" ]]; then
      if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$bin" >>"$GITHUB_PATH"
      else
        printf '%s\n' "$bin" >>"$GITHUB_PATH"
      fi
    fi
    ;;
  *)
    echo "no ffmpeg, and no installer for $os" >&2
    exit 1
    ;;
esac

if ! has_encoder; then
  echo "ffmpeg is installed but has no libx265 encoder" >&2
  exit 1
fi
