#!/usr/bin/env bash
# Make ffmpeg available for the synthetic Live Photo generator.
# The bytes that generator produces are hashed in the same run, so this does
# not need one exact binary. On Windows it pins one build by SHA-256 so a
# package feed outage does not fail CI. ExifTool stays the pinned metadata
# reader.
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
    # One pinned Gyan.dev essentials build from its GitHub release (the same
    # build the Chocolatey package fetches), checked against its SHA-256.
    # The Chocolatey community feed returned 503s often enough to fail CI.
    version="9.0.2"
    sha256="60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba"
    name="ffmpeg-${version}-essentials_build"
    url="https://github.com/GyanD/codexffmpeg/releases/download/${version}/${name}.zip"
    cache="${XDG_CACHE_HOME:-$HOME/.cache}/albumen"
    zip="${cache}/${name}.zip"
    bin="${cache}/${name}/bin"
    mkdir -p "$cache"
    if [[ ! -f "$zip" ]] || ! echo "${sha256}  ${zip}" | sha256sum -c - >/dev/null 2>&1; then
      curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 -o "${zip}.partial" "$url"
      echo "${sha256}  ${zip}.partial" | sha256sum -c - >&2
      mv "${zip}.partial" "$zip"
    fi
    echo "${sha256}  ${zip}" | sha256sum -c - >&2
    if [[ ! -x "${bin}/ffmpeg.exe" ]]; then
      rm -rf "${cache:?}/${name}"
      unzip -q "$zip" -d "$cache"
    fi
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
