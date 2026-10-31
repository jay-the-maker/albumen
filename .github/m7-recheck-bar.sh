#!/usr/bin/env bash
# M7 re-check bar (XO 02:53Z, 03:09Z): every "M7 RECHECK_DIAG files=" line
# must carry numeric recheck_p95_ms and recheck_max_ms fields (a missing or
# non-numeric field fails), with p95 <= 10 ms and max <= 100 ms over ALL
# samples (the first 2 s after each sync included). Needs >= 1 line.
# Call it with bash (it is not executable).
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
# XO 03:36Z (3): every "M7 WITHHELD files=" line must carry numeric
# withheld_p95_ms, withheld_max_ms (hand-over to applied, server side),
# ui_p95_ms and ui_max_ms (sync to the photo back, following the page's
# 200 ms poll), all <= 1000 ms. Needs >= 1 line.
awk '/^M7 WITHHELD files=/ {
       lines++
       delete v
       for (i = 1; i <= NF; i++) { n = index($i, "="); if (n) v[substr($i, 1, n - 1)] = substr($i, n + 1) }
       num = "^[0-9]+(\\.[0-9]+)?$"
       split("withheld_p95_ms withheld_max_ms ui_p95_ms ui_max_ms", k, " ")
       bad = 0
       for (j = 1; j <= 4; j++) {
         if (!(k[j] in v) || v[k[j]] !~ num) { printf "M7 WITHHELD_BAR MISSING_FIELD %s %s\n", k[j], $0; bad = 1 }
         else if (v[k[j]] + 0 > 1000) bad = 1
       }
       printf "M7 WITHHELD_BAR files=%s tombstones=%s withheld_p95=%s withheld_max=%s ui_p95=%s ui_max=%s %s\n", v["files"], v["tombstones"], v["withheld_p95_ms"], v["withheld_max_ms"], v["ui_p95_ms"], v["ui_max_ms"], bad ? "FAIL" : "ok"
       fail += bad
     }
     END { if (lines == 0) { print "M7 WITHHELD_BAR no WITHHELD lines"; exit 1 } exit fail > 0 }' "$log" \
  || { echo "::error::M7 withheld bar missed (withheld and UI p95 and max <= 1 s, all four fields required on every line)"; exit 1; }
