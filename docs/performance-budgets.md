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

The Android worker already refuses to run unless the device is idle, charging, and on a network. That is the default, not a measured battery percent. The percent and the CPU average stay targets until a client exists.

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

| Field | Meaning | Worse when |
| --- | --- | --- |
| `copy_verify_mb_s` | MB/s of the eight-file copy, 1 MB = 1,000,000 bytes | lower |
| `copy_verify_items_s` | files per second of that copy | lower |
| `search_cold_ms` | first `search` on a new library, including ExifTool | higher |
| `search_warm_p50_ms` | nearest-rank p50 of 20 warm searches, then the median across repeats | higher |
| `search_warm_p95_ms` | nearest-rank p95 of those 20, then the median across repeats | higher |
| `mcp_browse_p50_ms` | nearest-rank p50 of 20 `browse` calls, then the median across repeats | higher |

`--check` reads the baseline first and refuses to compare a different `search_items`, `copy_files`, or `copy_bytes_each`. A 380,000-item run must not be checked against this file.

Noise handling, applied to the five repeat scores before the comparison:

- If `(max - min) / median` is above 0.25, drop the worst sample. For throughput that is the lowest sample. For latency that is the highest.
- The score is then the median of what remains.
- Throughput fails when the score is below 90 percent of the baseline.
- Latency fails when the score is above 110 percent of the baseline.
- Exactly 10 percent is inside the gate. The comparison is exact at thousandths.

The committed baseline is the `ubuntu-24.04` measurement. A laptop uses `--measure`, which prints JSON and does not compare. `--write-baseline` replaces the file. Do not replace it to hide a real regression. Replace it when the runner itself moved, or when a change is an intentional speed trade that still keeps every byte and tag.

## Local 380,000-item run

This is how the full-library search and browse numbers are taken. It is not the CI job. It needs disk for 380,000 JPEGs and 380,000 XMP sidecars, and the cold search runs ExifTool across all of them.

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
ALBUMEN_BENCH_ITEMS=380000 cargo run --release --locked -p albumen-bench -- --measure
```

Do not set `ALBUMEN_BENCH_ITEMS` in CI. The workflow uses the default 128.
