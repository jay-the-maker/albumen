#!/bin/sh
# Fail unless every commit in BASE..HEAD has a Signed-off-by trailer.
set -eu

base="${BASE:?BASE is required}"

if ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
  echo "Base commit is not in this clone: $base" >&2
  exit 1
fi

commits=$(git rev-list "${base}..HEAD")
if [ -z "$commits" ]; then
  echo "No commits in ${base}..HEAD" >&2
  exit 1
fi

missing=0
for sha in $commits; do
  if ! git log -1 --format=%B "$sha" | grep -q '^Signed-off-by: '; then
    echo "Missing Signed-off-by: $(git log -1 --format='%h %s' "$sha")" >&2
    missing=1
  fi
done

if [ "$missing" -ne 0 ]; then
  echo "DCO check failed. Add Signed-off-by with git commit -s." >&2
  exit 1
fi

echo "DCO check passed"
