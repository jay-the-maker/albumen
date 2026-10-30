#!/usr/bin/env bash
# Run exactly one index_shared test under /usr/bin/time -v and fail unless
# it really ran and stayed under the peak RSS bar.
# With --exact a renamed test runs 0 tests and exits 0, so the step
# requires "test result: ok. 1 passed;" and the test's "M6 <tag> RESULT"
# line in the output.  Usage: m6-one.sh <test name> <tag>  (env passes through)
set -euo pipefail
name=$1 tag=$2
log=$(mktemp)
# Peak RSS bar (M6, XO 20:12Z): 332 MiB = 339968 kB, measured by GNU time -v.
# Linux reports the largest process in cargo's tree (the test binary).
max_kb=${ALBUMEN_M6_MAX_RSS_KB:-339968}
[ -x /usr/bin/time ] || { echo "::error::/usr/bin/time (GNU time) missing; cannot measure peak RSS"; exit 1; }
timelog=$(mktemp)
/usr/bin/time -v -o "$timelog" cargo test --release --locked -p albumen-ingest --test index_shared -- \
  --exact "$name" --nocapture --test-threads=1 2>&1 | tee "$log"
cat "$timelog"
rss=$(sed -n 's/^[[:space:]]*Maximum resident set size (kbytes): \([0-9]*\)$/\1/p' "$timelog")
if [ -z "$rss" ]; then
  echo "::error::no 'Maximum resident set size' line from /usr/bin/time -v"
  exit 1
fi
echo "M6 $tag PEAK_RSS_KB=$rss bar_kb=$max_kb"
if [ "$rss" -gt "$max_kb" ]; then
  echo "::error::$name peak RSS ${rss} kB over ${max_kb} kB (332 MiB)"
  exit 1
fi
if ! grep -q '^test result: ok\. 1 passed; 0 failed; 0 ignored' "$log"; then
  echo "::error::$name did not show exactly '1 passed' (renamed or filtered out?)"
  exit 1
fi
if ! grep -q "M6 $tag RESULT" "$log"; then
  echo "::error::$name printed no 'M6 $tag RESULT' line"
  exit 1
fi
