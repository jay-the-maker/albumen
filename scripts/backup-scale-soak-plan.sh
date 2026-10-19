#!/usr/bin/env bash
# Choose the shard count for the seeded backup soak.
# A pull request runs 8 synthetic items on one shard. The nightly run is
# 10,000 items. One runner does not finish that many faulted uploads after
# booting an emulator or a simulator, so those 10,000 are 10 shards of 1,000.
set -euo pipefail

event=${GITHUB_EVENT_NAME:-pull_request}
if [[ "$event" == "pull_request" ]]; then
  items=8
  shards=1
  seed=1
  smoke=1
else
  items=${ALBUMEN_SOAK_ITEMS_INPUT:-10000}
  shards=${ALBUMEN_SOAK_SHARDS_INPUT:-10}
  seed=${ALBUMEN_SOAK_SEED_INPUT:-}
  if [[ -z "$items" ]]; then
    items=10000
  fi
  if [[ -z "$shards" ]]; then
    shards=10
  fi
  if [[ -z "$seed" ]]; then
    seed=${GITHUB_RUN_ID:-1}
  fi
  smoke=0
fi

if (( items < 1 || shards < 1 || items % shards != 0 )); then
  echo "items ($items) must be a positive multiple of shards ($shards)" >&2
  exit 1
fi
per=$((items / shards))
matrix=$(python3 -c 'import json,sys; print(json.dumps(list(range(int(sys.argv[1])))))' "$shards")
{
  echo "items=$items"
  echo "shards=$shards"
  echo "seed=$seed"
  echo "per_shard=$per"
  echo "matrix=$matrix"
  echo "smoke=$smoke"
} >>"${GITHUB_OUTPUT:-/dev/null}"
echo "backup-scale-soak plan items=$items shards=$shards per_shard=$per seed=$seed smoke=$smoke"
