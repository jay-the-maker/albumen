# Performance budgets

Performance is at a premium. These are the numbers a change is measured against. A faster copy that drops a byte, a tag, or a checksum is a failure. Cost is outside these budgets.

Cold start, the first thumbnail, scroll, and backup CPU and battery are targets until the apps exist. Nothing in CI can fail them yet. Import throughput, metadata search, and MCP response time exist now. CI regression-gates those on a scaled library. The 380,000-item numbers are taken locally.

## Targets until the apps exist

| Metric | Budget |
| --- | --- |
| Cold start | ≤ 2.0 seconds from process start to the library shell |
| Time to first thumbnail | ≤ 1.0 second on a 380,000-item library |
| Scroll | ≥ 60 FPS on a 380,000-item library, which is a p95 frame time ≤ 16.7 ms |
| Backup CPU | Average ≤ 25% of one core across a background backup. A hash may use one core. It does not fan out across every core. |
| Backup battery | Backup does not run on battery by default. Once a client can measure drain, screen-off backup stays ≤ 5% battery per hour. |

The periodic Android backup does not run unless the device is idle, charging, and on a network. That is the battery default, not a measured battery percent. The first job after setup is the backup the user just started: a one-time network-constrained job aimed at a verified copy within 5 minutes. The percent and the CPU average stay targets. The worker hashes one file at a time.

## Budgets for the library

These are the product bars on a 380,000-item library. CI does not fail a job for missing the absolute number. CI fails a regression of the scaled run, described below.

| Metric | Budget |
| --- | --- |
| Search, warm | p50 ≤ 500 ms and p95 ≤ 1.5 s |
| Import | ≥ 100 MB/s on a local SSD, and ≥ 20 items/s for originals of about 5 MB |

1 MB is 1,000,000 bytes. 100 MB/s of 5 MB files is 20 items/s.

Warm metadata search still SHA-256s every file before it trusts the in-memory cache, and a cold search also runs ExifTool. A local run at 380,000 items is expected to miss the 500 ms bar until search stops re-reading every byte. That miss is the measurement. It is not a claim that today's code already hits the bar, and it is not a CI failure.

## What CI measures

`albumen-bench` is a repeated-sample harness in the same style as criterion: five repeats, then a median. The criterion crate is not a dependency. The job is `performance regression` in `.github/workflows/ci.yml`. It runs on `ubuntu-24.04` only, with the pinned ExifTool from `scripts/install-exiftool.sh`.

```bash
cargo run --release --locked -p albumen-bench -- --check
```

The default scale, and the scale stored in [bench/baseline.json](../bench/baseline.json), is:

- 128 library items. Each item is a synthetic JPEG (`FF D8 FF D9`) plus an XMP sidecar whose caption is `bench-caption`.
- 8 files copied through `copy_verify`, 4 MiB each, into a fresh destination directory per repeat.
- Search and browse go through `Library::call`, which is the MCP tool path. Export and upload stay off.

The report always prints raw rates and latencies: `copy_verify_mb_s` and `copy_verify_items_s` (1 MB = 1,000,000 bytes), `search_cold_ms`, nearest-rank `search_warm_p50_ms` and `search_warm_p95_ms` over 20 warm searches, and `mcp_browse_p50_ms` over 20 browse calls.

Those raw numbers are not the CI gate. Two `ubuntu-24.04` runners can differ by more than 10 percent on the same commit, which is machine noise, not a code change. Each repeat also times a reference on that same machine:

- a SHA-256 while the bytes are copied, an fsync, and a SHA-256 of the destination
- one ExifTool read of the same library
- a stat, a SHA-256, and a JSON object for every library file
- a directory walk that stats every file, builds a JSON object, and sorts those objects

The gated fields are Albumen divided into that reference. Higher is better. A slower Albumen lowers the ratio even when the runner is faster or slower than the last job.

| Field | Ratio | CI failure |
| --- | --- | --- |
| `copy_verify_vs_plain` | `copy_verify` MB/s ÷ reference copy MB/s | yes |
| `search_cold_vs_exif` | ExifTool read time ÷ cold search time | yes |
| `browse_vs_walk` | directory walk ÷ browse p50 | yes |
| `search_warm_p50_vs_hash` | stat-and-hash pass ÷ warm search p50 | no |
| `search_warm_p95_vs_hash` | stat-and-hash pass ÷ warm search p95 | no |

Warm p50 and p95 are still printed on every run, including the local 380,000-item run. They are not a CI failure. On `ubuntu-24.04` that ratio has come out near 0.13 and near 0.61 for this same benchmark. That spread is the runner, not a code change. Cold search, the copy, and browse stay inside 10 percent on those same runners, so those three are the gate.

`--check` reads the baseline first and refuses to compare a different `search_items`, `copy_files`, or `copy_bytes_each`. A 380,000-item run must not be checked against this file.

Noise handling, applied to the five repeat ratios before the comparison:

- If `(max - min) / median` is above 0.25, drop the worst sample. That is the lowest ratio.
- The score is then the median of what remains.
- The score fails when it is below 90 percent of the baseline.
- Exactly 10 percent is inside the gate. The comparison is exact at thousandths.

The committed ratios are the `ubuntu-24.04` measurement. A laptop uses `--measure`, which prints JSON and does not compare. `--write-baseline` replaces the file. Do not replace it to hide a real regression. Replace it when the relationship to the reference moved for a reason other than the code, or when a change is an intentional speed trade that still keeps every byte and tag.

## Local 380,000-item run

This is how the full-library search and browse numbers are taken. It is not the CI job. It needs disk for 380,000 JPEGs and 380,000 XMP sidecars, and the cold search runs ExifTool across all of them.

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
ALBUMEN_BENCH_ITEMS=380000 cargo run --release --locked -p albumen-bench -- --measure
```

Do not set `ALBUMEN_BENCH_ITEMS` in CI. The workflow uses the default 128.

## Takeout import, 100,000 items

This is the switching-time measurement for `albumen import-takeout`. It is not the performance-regression job. That job does not pass `--takeout` and does not set `ALBUMEN_TAKEOUT_ITEMS`.

The fixture is generated in a stored zip: 50,000 tiny JPEG members and 50,000 tiny JSON sidecars, 100,000 archive members in all. No real photo is used. The clock starts at `import_takeout` and stops when it returns. Generation is not included. Reconciliation is 100 percent when `count_in` and `count_out` are both 100,000 and `mismatched` is 0.

The local budget is 4 hours or less for those 100,000 members.

```bash
cargo run --release --locked -p albumen-bench -- --takeout
```

`ALBUMEN_TAKEOUT_ITEMS` overrides the member count. The default is 100000. A smaller count is only for a smoke check. The budget stays 4 hours, which is the budget for 100,000 members.

The command prints JSON. `seconds` is the import time. `reconciled` is true only at 100 percent. The process exits non-zero when reconciliation is short of that or when `seconds` is over 14400.

A pull request does not run this. It is too long for the ingest matrix. [`.github/workflows/takeout-scale.yml`](../.github/workflows/takeout-scale.yml) runs it on `workflow_dispatch` and on a weekly schedule.

The same budget covers an iCloud export of 100,000 generated files. `--icloud` times `import_icloud` on a directory of tiny still/video pairs. Generation is not included. The workflow runs that command in a second job. `ALBUMEN_TAKEOUT_ITEMS` overrides the count for either command.

A local run of the default command, on this machine's ext4 filesystem, imported 100,000 members in 27.992 seconds. `count_in` and `count_out` were 100000 and `mismatched` was 0, so reconciliation was 100 percent. That is inside the 4 hour budget. The JSON field was `seconds`: 27.992430546.
