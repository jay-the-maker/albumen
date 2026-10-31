#!/usr/bin/env bash
# Run exactly one test (default: albumen-ingest's index_shared; set
# M6_PKG/M6_TEST for another, e.g. albumen/gui_index_x1) under /usr/bin/time -v and fail unless
# it really ran and stayed under the peak RSS bar.
# With --exact a renamed test runs 0 tests and exits 0, so the step
# requires "test result: ok. 1 passed;" and the test's "M6 <tag> RESULT"
# line in the output.  Usage: m6-one.sh <test name> <tag>  (env passes through)
set -euo pipefail
name=$1 tag=$2
# INVALID counter (XO 22:13Z): one line per attempt, plus GREEN_AFTER_RERUN
# when a rerun passes after INVALID. Uploaded as a CI artifact.
counter_log=${M6_COUNTER_LOG:-$PWD/m6-invalid-counter.log}
m6_count() { # outcome attempt max_gap_ms
  local line
  line="$(date -u +%Y-%m-%dT%H:%M:%SZ) tag=$tag attempt=$2 outcome=$1 sha=$(git rev-parse --short=12 HEAD 2>/dev/null || echo unknown) runner=${RUNNER_NAME:-$(hostname)} max_gap_ms=$3"
  echo "$line" >> "$counter_log"
  echo "M6 INVALID_COUNTER $line"
}
# At most one rerun after INVALID (XO 22:03Z): the outer call runs the
# attempt itself twice at most; two INVALID runs in a row are a FAIL.
if [ -z "${M6_ATTEMPT:-}" ]; then
  arc=0
  M6_ATTEMPT=1 bash "$0" "$@" || arc=$?
  if [ "$arc" -eq 75 ]; then
    echo "M6 $tag attempt 1 INVALID; one rerun allowed"
    arc=0
    M6_ATTEMPT=2 bash "$0" "$@" || arc=$?
    [ "$arc" -eq 0 ] && m6_count GREEN_AFTER_RERUN 2 "$(cat "$counter_log.gap" 2>/dev/null || echo NA)"
    if [ "$arc" -eq 75 ]; then
      echo "M6 $tag FAIL: two INVALID runs in a row"
      echo "::error::$name FAIL: INVALID twice in a row (box froze on both runs)"
      rm -f "$counter_log.gap"
      exit 1
    fi
  fi
  rm -f "$counter_log.gap"
  exit "$arc"
fi
log=$(mktemp)
gap=NA
# Every attempt records its outcome by exit code: 0 PASS, 75 INVALID, else FAIL.
trap 'e=$?; case $e in 0) o=PASS;; 75) o=INVALID;; *) o=FAIL;; esac; echo "$gap" > "$counter_log.gap"; m6_count $o "$M6_ATTEMPT" "$gap"' EXIT
# Peak RSS bar (M6, XO 20:12Z): 332 MiB = 339968 kB, measured by GNU time -v.
# Linux reports the largest process in cargo's tree (the test binary).
max_kb=${ALBUMEN_M6_MAX_RSS_KB:-339968}
[ -x /usr/bin/time ] || { echo "::error::/usr/bin/time (GNU time) missing; cannot measure peak RSS"; exit 1; }
timelog=$(mktemp)
# Independent freeze ticker (XO 22:06Z): every 0.2 s log wall time and
# monotonic-ish time (/proc/uptime). A gap over 1 s, or wall and uptime
# drifting apart by over 1 s, makes the run INVALID (exit 75): rerun it,
# it is neither a pass nor a product FAIL.
ticks=$(mktemp)
( while :; do echo "$(date +%s.%N) $(cut -d' ' -f1 /proc/uptime)"; sleep 0.2; done ) > "$ticks" &
ticker=$!
rc=0
/usr/bin/time -v -o "$timelog" cargo test --release --locked -p "${M6_PKG:-albumen-ingest}" ${M6_FEATURES:+--features "$M6_FEATURES"} --test "${M6_TEST:-index_shared}" -- \
  --exact "$name" --nocapture --test-threads=1 2>&1 | tee "$log" || rc=$?
kill "$ticker" 2>/dev/null || true
wait "$ticker" 2>/dev/null || true
# Max gap with its wall-clock bounds (UTC epoch ms of the tick before and after).
read -r gap skew gs ge < <(awk 'NR==1{w0=$1;u0=$2} NR>1{g=($1-pw)*1000; if(g>mg){mg=g;gs=pw;ge=$1}} {pw=$1; s=(($1-w0)-($2-u0))*1000; if(s<0)s=-s; if(s>ms)ms=s} END{printf "%.1f %.1f %.0f %.0f\n", mg+0, ms+0, gs*1000, ge*1000}' "$ticks")
iso_ms() { [ "$1" -gt 0 ] 2>/dev/null && date -u -d "@$(( $1 / 1000 )).$(printf %03d $(( $1 % 1000 )))" +%Y-%m-%dT%H:%M:%S.%3NZ || echo NA; }
echo "M6 $tag FREEZE_TICKER max_gap_ms=$gap wall_vs_uptime_skew_ms=$skew max_gap_start_epoch_ms=$gs max_gap_end_epoch_ms=$ge max_gap_start_utc=$(iso_ms "$gs") max_gap_end_utc=$(iso_ms "$ge")"
# A freeze voids timing only: if the test failed and did not get past its
# correctness asserts (no "M6 <tag> CORRECTNESS_OK"), that is a real FAIL.
froze=0
if grep -q "M6 $tag INVALID" "$log" || awk -v g="$gap" -v s="$skew" 'BEGIN{exit !(g>1000 || s>1000)}'; then
  froze=1
fi
if [ "$froze" -eq 1 ] && [ "$rc" -ne 0 ] && ! grep -q "M6 $tag CORRECTNESS_OK" "$log"; then
  echo "::error::$name FAIL during a freeze: a correctness or RSS assert failed before INVALID could apply (exit $rc)"
  exit "$rc"
fi
if [ "$froze" -eq 1 ]; then
  echo "M6 $tag INVALID attempt=$M6_ATTEMPT (box froze: max_gap_ms=$gap skew_ms=$skew); timings are not evidence, rerun"
  echo "::error::$name INVALID run (freeze); rerun, not a pass"
  exit 75
fi
if [ "$rc" -ne 0 ]; then
  echo "::error::$name failed (exit $rc)"
  exit "$rc"
fi
cat "$timelog"
rss=$(sed -n 's/^[[:space:]]*Maximum resident set size (kbytes): \([0-9]*\)$/\1/p' "$timelog")
if [ -z "$rss" ]; then
  echo "::error::no 'Maximum resident set size' line from /usr/bin/time -v"
  exit 1
fi
echo "M6 $tag PEAK_RSS_KB=$rss bar_kb=$max_kb"
# M6_TREE_RSS=diag (X1, XO 20:58Z): the tree peak is printed but the bar is
# on the product process, asserted in the test and via M6_RESULT_MAX.
if [ "${M6_TREE_RSS:-bar}" = diag ]; then
  echo "M6 $tag tree peak RSS is a diagnostic here (bar is on the product child)"
elif [ "$rss" -gt "$max_kb" ]; then
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
# Optional extra bars on RESULT fields, space-separated key=limit pairs,
# e.g. M6_RESULT_MAX="index_read_max_ms=100 gui_child_peak_rss_kb=339968".
for bar in ${M6_RESULT_MAX:-}; do
  key=${bar%%=*} lim=${bar#*=}
  val=$(grep "M6 $tag RESULT" "$log" | tail -1 | tr ' ' '\n' | sed -n "s/^$key=//p")
  [ -n "$val" ] || { echo "::error::no $key in the M6 $tag RESULT line"; exit 1; }
  echo "M6 $tag $key=$val bar=$lim"
  awk -v v="$val" -v l="$lim" 'BEGIN{exit !(v ~ /^[0-9.]+$/ && v+0 <= l+0)}' \
    || { echo "::error::$key $val over $lim"; exit 1; }
done
