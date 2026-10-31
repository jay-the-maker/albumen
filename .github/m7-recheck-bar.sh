#!/usr/bin/env bash
# M7 re-check bar (XO 02:53Z, 03:09Z): every "M7 RECHECK_DIAG files=" line
# must carry numeric recheck_p95_ms and recheck_max_ms fields (a missing or
# non-numeric field fails), with p95 <= 10 ms and max <= 100 ms over ALL
# samples (the first 2 s after each sync included). Needs >= 1 line.
set -euo pipefail
log=${1:?usage: m7-recheck-bar.sh m7-recheck.log}
awk '/^M7 RECHECK_DIAG files=/ {
       lines++
       delete v
       for (i = 1; i <= NF; i++) { n = index($i, "="); if (n) v[substr($i, 1, n - 1)] = substr($i, n + 1) }
       num = "^[0-9]+(\\.[0-9]+)?$"
       if (!("recheck_p95_ms" in v) || !("recheck_max_ms" in v) || v["recheck_p95_ms"] !~ num || v["recheck_max_ms"] !~ num) {
         printf "M7 RECHECK_BAR MISSING_FIELD %s\n", $0; fail++; next
       }
       bad = (v["recheck_p95_ms"] + 0 > 10 || v["recheck_max_ms"] + 0 > 100)
       printf "M7 RECHECK_BAR files=%s tombstones=%s p95=%s max=%s %s\n", v["files"], v["tombstones"], v["recheck_p95_ms"], v["recheck_max_ms"], bad ? "FAIL" : "ok"
       fail += bad
     }
     END { if (lines == 0) { print "M7 RECHECK_BAR no RECHECK_DIAG lines"; exit 1 } exit fail > 0 }' "$log" \
  || { echo "::error::M7 re-check bar missed (p95 <= 10 ms and max <= 100 ms over all samples, both fields required on every line)"; exit 1; }
