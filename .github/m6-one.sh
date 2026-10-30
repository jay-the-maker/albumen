#!/usr/bin/env bash
# Run exactly one index_shared test and fail unless it really ran.
# With --exact a renamed test runs 0 tests and exits 0, so the step
# requires "test result: ok. 1 passed;" and the test's "M6 <tag> RESULT"
# line in the output.  Usage: m6-one.sh <test name> <tag>  (env passes through)
set -euo pipefail
name=$1 tag=$2
log=$(mktemp)
cargo test --release --locked -p albumen-ingest --test index_shared -- \
  --exact "$name" --nocapture --test-threads=1 2>&1 | tee "$log"
if ! grep -q '^test result: ok\. 1 passed; 0 failed; 0 ignored' "$log"; then
  echo "::error::$name did not show exactly '1 passed' (renamed or filtered out?)"
  exit 1
fi
if ! grep -q "M6 $tag RESULT" "$log"; then
  echo "::error::$name printed no 'M6 $tag RESULT' line"
  exit 1
fi
