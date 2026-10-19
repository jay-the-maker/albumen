#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Optional rung 4 helper. CI does not run this. chmod a-w is not a substitute:
# this profile denies network and denies writes outside the manifest parent.

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macos-readonly-sandbox: not macOS; nothing to run"
  exit 0
fi

if [[ $# -ne 2 ]]; then
  echo "usage: scripts/macos-readonly-sandbox.sh SOURCE MANIFEST" >&2
  exit 2
fi

source="$1"
manifest="$2"
if [[ ! -d "$source" ]]; then
  echo "macos-readonly-sandbox: source is not a directory: $source" >&2
  exit 1
fi

manifest_parent="$(python3 - "$source" "$manifest" <<'PY'
import os, sys
source, manifest = sys.argv[1], sys.argv[2]
source = os.path.realpath(source)
parent = os.path.dirname(os.path.abspath(manifest))
cursor = parent
missing = []
while not os.path.exists(cursor):
    name = os.path.basename(cursor)
    missing.append(name)
    nxt = os.path.dirname(cursor)
    if nxt == cursor:
        raise SystemExit("macos-readonly-sandbox: cannot resolve the manifest")
    cursor = nxt
resolved = os.path.realpath(cursor)
for part in reversed(missing):
    resolved = os.path.join(resolved, part)
if resolved == source or resolved.startswith(source + os.sep):
    raise SystemExit(f"macos-readonly-sandbox: refusing manifest inside {source}")
print(os.path.dirname(resolved))
PY
)"
mkdir -p "$manifest_parent"

profile="$(mktemp)"
trap 'rm -f "$profile"' EXIT
cat > "$profile" <<EOF
(version 1)
(deny default)
(allow process*)
(allow signal)
(allow sysctl-read)
(allow mach*)
(allow ipc*)
(allow file-read*)
(allow file-write* (subpath "$manifest_parent"))
(deny network*)
EOF

repo="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$repo/target/release:$PATH"
sandbox-exec -f "$profile" albumen scan "$source" --manifest "$manifest"
echo "macos-readonly-sandbox: scan finished"
