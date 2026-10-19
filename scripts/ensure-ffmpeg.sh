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
    choco install ffmpeg -y --no-progress
    if [[ -n "${GITHUB_PATH:-}" ]]; then
      printf '%s\n' "/c/ProgramData/chocolatey/bin" >> "$GITHUB_PATH"
    fi
    export PATH="/c/ProgramData/chocolatey/bin:${PATH}"
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
