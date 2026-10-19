#!/bin/sh
# Prove the default server.json mount of /library cannot change originals.
# The mount string comes from server.json. Create, modify, rename, and delete
# must fail inside the container, and the host directory must stay unchanged.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
image=${1:-albumen-mcp:ci}
cd "$root"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required to check the library mount" >&2
  exit 1
fi

mount_value=$(python3 scripts/check-mcp-release.py --print-library-mount)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf 'original-bytes' > "$work/photo.jpg"

case "$work" in
  *,*)
    echo "temporary path cannot be substituted into a docker mount: $work" >&2
    exit 1
    ;;
esac

spec=$(python3 -c 'import sys; print(sys.argv[1].replace("{library_path}", sys.argv[2]), end="")' "$mount_value" "$work")
case "$spec" in
  *"{library_path}"*)
    echo "library mount still contains {library_path}" >&2
    exit 1
    ;;
esac

before=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/photo.jpg")

docker run --rm --network none \
  --mount "$spec" \
  --entrypoint /bin/sh \
  "$image" \
  -c '
set -eu
photo=/library/photo.jpg
content=$(cat "$photo")
if [ "$content" != "original-bytes" ]; then
  echo "library file is not the host original" >&2
  exit 1
fi
if touch /library/created.txt; then
  echo "create succeeded" >&2
  exit 1
fi
if mkdir /library/newdir; then
  echo "create directory succeeded" >&2
  exit 1
fi
if sh -c "printf extra >> /library/photo.jpg"; then
  echo "modify succeeded" >&2
  exit 1
fi
if mv /library/photo.jpg /library/renamed.jpg; then
  echo "rename succeeded" >&2
  exit 1
fi
if rm /library/photo.jpg; then
  echo "delete succeeded" >&2
  exit 1
fi
content=$(cat "$photo")
if [ "$content" != "original-bytes" ]; then
  echo "library file changed inside the container" >&2
  exit 1
fi
if [ -e /library/created.txt ] || [ -e /library/renamed.jpg ] || [ -d /library/newdir ]; then
  echo "library gained a name" >&2
  exit 1
fi
'

after=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/photo.jpg")
if [ "$before" != "$after" ]; then
  echo "host file changed" >&2
  exit 1
fi
set -- "$work"/*
if [ "$#" -ne 1 ] || [ "$(basename "$1")" != "photo.jpg" ]; then
  echo "host tree changed" >&2
  exit 1
fi

echo "default /library mount cannot change originals ($image)"
