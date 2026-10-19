#!/bin/sh
# Pack albumen-mcp.mcpb with the official @anthropic-ai/mcpb CLI.
# The manifest asks for a library directory and passes --library only.
# Export and upload stay off. SHA256SUMS is written beside the bundle.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$root"

expected="albumen-mcp-linux-x86_64
albumen-mcp-linux-aarch64
albumen-mcp-darwin-arm64
albumen-mcp-darwin-x86_64
albumen-mcp-win32-x86_64.exe
albumen-mcp-win32-aarch64.exe"

mcpb() {
  if [ -n "${MCPB_BIN:-}" ]; then
    "$MCPB_BIN" "$@"
  else
    npx --yes @anthropic-ai/mcpb@2.1.2 "$@"
  fi
}

cargo_version() {
  python3 - <<'PY'
from pathlib import Path
for line in Path("mcp/Cargo.toml").read_text().splitlines():
    stripped = line.strip()
    if stripped.startswith("version = "):
        print(stripped.split("=", 1)[1].strip().strip('"'))
        break
else:
    raise SystemExit("mcp/Cargo.toml has no version")
PY
}

usage() {
  echo "usage: scripts/pack-mcpb.sh --self-test" >&2
  echo "       scripts/pack-mcpb.sh VERSION BIN_DIR OUTPUT.mcpb" >&2
  exit 2
}

pack_one() {
  version=$1
  bin_dir=$2
  output=$3
  have=$(cargo_version)
  if [ "$version" != "$have" ]; then
    echo "version $version does not match mcp/Cargo.toml ($have)" >&2
    return 1
  fi
  for name in $expected; do
    if [ ! -f "$bin_dir/$name" ]; then
      echo "missing binary: $bin_dir/$name" >&2
      return 1
    fi
  done

  stage=$(mktemp -d)
  mkdir -p "$stage/server"
  for name in $expected; do
    cp "$bin_dir/$name" "$stage/server/$name"
    chmod 755 "$stage/server/$name"
  done
  cp mcpb/albumen-mcp "$stage/server/albumen-mcp"
  chmod 755 "$stage/server/albumen-mcp"

  VERSION="$version" STAGE="$stage" python3 - <<'PY'
import json
import os
from pathlib import Path

version = os.environ["VERSION"]
stage = Path(os.environ["STAGE"])
template = Path("mcpb/manifest.json").read_text()
banned = (
    "enable-export",
    "enable_export",
    "enable-upload",
    "enable_upload",
    "--apply",
    "--execute",
)
for word in banned:
    if word in template:
        raise SystemExit(f"mcpb/manifest.json contains {word}")
manifest = json.loads(template)
args = ["--library", "${user_config.library}"]
mcp = manifest["server"]["mcp_config"]
if mcp["args"] != args:
    raise SystemExit("mcp_config.args must be --library only")
win32 = mcp["platform_overrides"]["win32"]
if win32.get("args") != args:
    raise SystemExit("win32 args must be --library only")
if not win32["command"].endswith("albumen-mcp-win32-x86_64.exe"):
    raise SystemExit("win32 command must be the x86_64 executable")
library = manifest["user_config"]["library"]
if library["type"] != "directory" or library.get("required") is not True:
    raise SystemExit("library user_config must be a required directory")
if list(manifest["user_config"]) != ["library"]:
    raise SystemExit("user_config must be the library directory only")
names = [tool["name"] for tool in manifest["tools"]]
if names != ["search", "browse"]:
    raise SystemExit("tools must be search and browse")
manifest["version"] = version
text = json.dumps(manifest, indent=2) + "\n"
for word in banned:
    if word in text:
        raise SystemExit(f"generated manifest contains {word}")
(stage / "manifest.json").write_text(text)
PY

  mcpb validate "$stage"
  output_dir=$(CDPATH= cd -- "$(dirname "$output")" && pwd)
  output_abs="$output_dir/$(basename "$output")"
  rm -f "$output_abs"
  mcpb pack "$stage" "$output_abs"

  OUTPUT="$output_abs" VERSION="$version" python3 - <<'PY'
import json
import os
import zipfile

path = os.environ["OUTPUT"]
version = os.environ["VERSION"]
banned = (
    "enable-export",
    "enable_export",
    "enable-upload",
    "enable_upload",
    "--apply",
    "--execute",
)
expected = [
    "server/albumen-mcp",
    "server/albumen-mcp-linux-x86_64",
    "server/albumen-mcp-linux-aarch64",
    "server/albumen-mcp-darwin-arm64",
    "server/albumen-mcp-darwin-x86_64",
    "server/albumen-mcp-win32-x86_64.exe",
    "server/albumen-mcp-win32-aarch64.exe",
]
with zipfile.ZipFile(path) as bundle:
    names = bundle.namelist()
    for name in expected:
        if name not in names:
            raise SystemExit(f"{path} is missing {name}")
    raw = bundle.read("manifest.json").decode()
    for word in banned:
        if word in raw:
            raise SystemExit(f"packed manifest contains {word}")
    manifest = json.loads(raw)
    if manifest["version"] != version:
        raise SystemExit(f"packed version {manifest['version']} != {version}")
    info = bundle.getinfo("server/albumen-mcp")
    mode = (info.external_attr >> 16) & 0o777
    if mode & 0o111 == 0:
        raise SystemExit(f"server/albumen-mcp is not executable in {path} (mode {mode:o})")
PY

  (
    cd "$output_dir"
    sha256sum "$(basename "$output")" > SHA256SUMS
    sha256sum -c SHA256SUMS
  )
  rm -rf "$stage"
}

self_test() {
  version=$(cargo_version)
  bins=$(mktemp -d)
  out=$(mktemp -d)
  missing=$(mktemp -d)
  for name in $expected; do
    printf '#!/bin/sh\nexit 0\n' > "$bins/$name"
    chmod 755 "$bins/$name"
    cp "$bins/$name" "$missing/$name"
  done
  rm "$missing/albumen-mcp-linux-aarch64"
  pack_one "$version" "$bins" "$out/albumen-mcp.mcpb"
  python3 - <<'PY'
import json
from pathlib import Path
data = json.loads(Path("mcpb/manifest.json").read_text())
if data["version"] != "0.0.0":
    raise SystemExit("pack rewrote mcpb/manifest.json")
PY
  if sh "$0" "$version" "$missing" "$out/should-fail.mcpb"; then
    echo "missing binary was packed" >&2
    exit 1
  fi
  rm -rf "$bins" "$out" "$missing"
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit 0
fi

if [ "$#" -ne 3 ]; then
  usage
fi

pack_one "$1" "$2" "$3"
