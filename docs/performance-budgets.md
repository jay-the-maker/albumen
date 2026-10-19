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

## Backup verified percent

The reliability target is 99.9 percent of items verified within 24 hours. That window includes passes that meet a receiver that is down, a connection that drops, a kill during an upload, and a SHA-256 mismatch. A miss still has to be visible: every item that is still unverified is named in the status file or in notification 1001.

`backup soak` in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) is the measured stand-in. It runs `scripts/android-soak.sh` on `ubuntu-24.04`. The harness is `BackupSoakTest`, which drives `executeBackup` and the real `WebDavUploader` against a loopback receiver. The library is 24 synthetic items, 256 KiB each. They are not photographs. Two sequences each run five cycles: receiver down (a closed port), network drop (the socket closes after the first 64 bytes of the PUT body), kill mid-upload (that same close, and the pass stops so later items resume on the next cycle), SHA-256 mismatch (HTTP 201 with a digest that is not the bytes just sent), then a healthy recovery cycle. That is 10 cycles. A silent failure is an item the cycle tried to upload that is still unverified at the end of the cycle and whose filename is absent from that cycle's status file and from notification 1001. The status file is replaced on each publish, so the test keeps each cycle's text.

This run of the harness printed:

```text
backup-soak verified_percent=100.0 verified=24 items=24 cycles=10 silent_failures=0 fault_window_verified_percent=0.0
```

100.0 percent is 24 of 24 items verified after the recovery cycles. `silent_failures=0`. The fault cycles themselves verified 0.0 percent (0 of 24). Each of those 24 filenames was in the status file or notification 1001 before recovery started. The source bytes for each item were unchanged. The job fails when a silent failure appears or when recovery leaves an item unverified.

CI does not measure these parts of the 24-hour target:

- A real 24-hour wall clock. The harness finishes in one unit-test run.
- iOS background `URLSession`. The simulator job uploads with a foreground session. This soak is the Android receiver harness.
- OEM battery killers that freeze or stop the process and never open a later retry window.
- Battery percent drain, thermal throttling, and a physical camera roll.
- A true `SIGKILL`. The kill cycle closes the socket and sets the in-process stop flag. A process that is already gone cannot publish; the following cycle is what verifies the item or names it.
- A radio drop beyond a closed TCP socket. Receiver down is connection refused on a closed port. Network drop is that socket closing during the body.

[`.github/workflows/backup-scale-soak.yml`](../.github/workflows/backup-scale-soak.yml) is the longer stand-in. A pull request runs 8 synthetic items on one emulator shard and one simulator shard, seed 1. The nightly run is 10,000 synthetic items as 10 shards of 1,000, because one runner's 90 minute limit does not hold image boot plus a faulted upload of all 10,000. Each shard logs its seed and the fault for every item. Shards run in parallel. `time_to_99_9_ms` is that shard's clock from the start of the shard until 99.9 percent of its items are verified. The job summary reports the slowest of those clocks. The bytes are not photographs. The camera roll and any other member's namespace stay closed. What a 24 hour run on a real phone would still have to measure is the protocol in [test-ladder.md](test-ladder.md). That protocol is waiting on approval and does not run.

## Budgets for the library

These are the product bars on a 380,000-item library. CI does not fail a job for missing the absolute number. CI fails a regression of the scaled run, described below.

| Metric | Budget |
| --- | --- |
| Search, warm | p50 ≤ 500 ms and p95 ≤ 1.5 s |
| Import | ≥ 100 MB/s on a local SSD, and ≥ 20 items/s for originals of about 5 MB |
| Import vs raw write | Target ≥ 80% of a same-disk durable write for 1,000 files of about 5 MB |

1 MB is 1,000,000 bytes. 100 MB/s of 5 MB files is 20 items/s. The 80% bar is import MB/s divided by a raw write that uses the same per-file `fsync` and directory `fsync`. It is measured below. The absolute 100 MB/s bar stays a product target. CI does not fail the ratio. Two runs on one disk moved the raw write enough to move the ratio, and the ratio itself is below 80% because the import hashes every byte twice.

## Camera RAW verify

`albumen-receive` re-reads a stored file and SHA-256s those bytes before it reports the copy. CI uploads a synthetic 100 MB RAW-sized file (100,000,000 bytes, generated in the test, not a photograph and not committed) over WebDAV and over FTP. The clock is that re-read. It fails when the re-read takes more than 5 seconds. The source file's bytes and mtime stay as they were. The dev and test profiles compile the SHA-256 crates optimized so this clock is the hash, which is how a release binary already builds them.

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

## Real-size import throughput

`import throughput` in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) generates 1,000 synthetic files of 5,000,000 bytes each and imports them with `copy_verify`. The bytes start with a JPEG SOI and APP0 marker and end with EOI. They are deterministic and they are not committed. Generation is not part of the import clock.

The destination is a directory on the same runner. After the import, the harness removes that directory. It is a directory this process just created. The source directory stays. Each source is marked read-only before the import. Reconciliation is 100 percent only when `count_in` and `count_out` are both 1,000, `mismatched` is 0, and a second SHA-256 plus the length and mtime of every source still match the snapshot taken before `copy_verify`.

The raw write creates the same number of new files, of the same size, with the same durability calls: `File::sync_all` on the file and `dest::sync_dir` on the directory after each file. It does not hash and it does not preserve a source mtime. 1 MB is 1,000,000 bytes. The ratio is import MB/s divided by that raw MB/s. The job fails when reconciliation is short of 100 percent. A ratio below 0.800 is printed and is not a failure. The runs below say why.

```bash
cargo run --release --locked -p albumen-bench -- --throughput
```

The command prints JSON. `ALBUMEN_THROUGHPUT_FILES` and `ALBUMEN_THROUGHPUT_BYTES` override the size. A value below the CI default is refused unless `ALBUMEN_THROUGHPUT_ALLOW_SMALL=1`. CI does not set that variable.

The legs are `ubuntu-24.04`, `ubuntu-26.04`, `macos-14`, and `windows-2025`. Each leg prints `import-throughput` with its ratio. The job fails when reconciliation is short of 100 percent. It does not fail when the ratio is below 0.80.

Two back-to-back runs on this machine's ext4 disk, 1,000 files of 5,000,000 bytes, both reconciled 1,000 of 1,000 with `source_changed` 0:

| Run | Import MB/s | Raw MB/s | Ratio |
| --- | --- | --- | --- |
| 1 | 280.480 | 602.377 | 0.4656 |
| 2 | 279.289 | 737.715 | 0.3786 |

Import stayed near 280 MB/s. The raw write moved, and the ratio moved with it. Both ratios are below 0.80. `copy_verify` hashes the bytes while it copies them, hashes the destination again, and fsyncs the preserved mtime. The raw write does none of those. An 80% failure would be red on every runner, and a tight floor on the ratio would flake when the raw write is faster.

Proposed gate, after each leg below has a recorded ratio: fail that leg when a later run is under 80% of the recorded ratio for that same leg. That leaves room for the raw-write swing above. It is not applied in this job. The CI log for each leg is the measurement to record next to this table. `windows-latest` is not a separate leg.

The runner numbers are filled in from the pull-request job:

| Leg | Import MB/s | Raw MB/s | Ratio | Reconciliation |
| --- | --- | --- | --- | --- |
| ubuntu-24.04 | pending | pending | pending | pending |
| ubuntu-26.04 | pending | pending | pending | pending |
| macos-14 | pending | pending | pending | pending |
| windows-2025 | pending | pending | pending | pending |

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
