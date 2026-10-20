# Performance budgets

Performance is at a premium. These are the numbers a change is measured against. A faster copy that drops a byte, a tag, or a checksum is a failure. Cost is outside these budgets.

Cold start, the first thumbnail, scroll, and backup CPU and battery are targets until the apps exist. Nothing in CI can fail them yet. Import throughput, metadata search, and MCP response time exist now. CI regression-gates those on a scaled library. The 380,000-item numbers are taken locally.

## Targets until the apps exist

| Metric | Budget |
| --- | --- |
| Cold start | ≤ 2.0 seconds from process start to the library shell |
| Time to first thumbnail (app first paint) | ≤ 1.0 second on a 380,000-item library: the ceiling for the app's first painted thumbnail, from launch. Team target. |
| First thumbnail (library scale) | ≤ 300 ms at 380,000 items: `albumen-mcp` `thumbnail`, request to bytes and spawn to bytes, measured by the `library scale` job. Team target. |
| Scroll | ≥ 60 FPS on a 380,000-item library, which is a p95 frame time ≤ 16.7 ms |
| Import CPU | ≤ 2.0 normalized CPU seconds per GB imported on every leg, scored; the `import throughput` job fails when the best of 6 rounds is above 3.0 normalized CPU seconds per GB on any leg, and on Linux and Windows also when the median is above 3.0. macOS prints and scores its median and spread without gating on them. Only the two hash phases are normalized, to a 1,800 MB/s SHA-256 reference. Raw CPU seconds per GB and `cpu_percent` are reported next to it. Team targets. Status: Partial until every leg is at or under 2.0. See [Import CPU](#import-cpu). |
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
| Import vs raw write | Target ≥ 80% of a same-disk durable write for 1,000 files of about 5 MB, on `ubuntu-24.04`, `ubuntu-26.04`, and `windows-2025` |
| Import vs copy ceiling (`macos-14`) | Team target: import ≥ 90% of the copy ceiling measured in the same job, under per-file plus directory `F_FULLFSYNC`. The ratio to the raw write is still reported. |

1 MB is 1,000,000 bytes. 100 MB/s of 5 MB files is 20 items/s. The 80% bar is import MB/s divided by a raw write that uses the same per-file `fsync` and directory `fsync`. It is measured below. The absolute 100 MB/s bar stays a product target. CI does not fail a ratio below 0.80. A per-leg regression gate, when one is recorded, is a separate check and is not this 80% bar.

### macOS target: 90% of the same-job copy ceiling

This is a team target, adopted by the XO on 2026-10-19. It is not a product requirement from Jay.

On `macos-14`, the import target is **import MB/s at 90% or more of the copy ceiling measured in the same job**, under the current durability: `F_FULLFSYNC` on every file and on its directory. The copy ceiling uses the import's workers and syncs and reads each source once, with no hash and no read-back. The import throughput job prints it as `copy_ceiling_mb_s` and `copy_ceiling_ratio`, added in [#110](https://github.com/jay-the-maker/albumen/pull/110), which is on main. The other legs keep the 0.80-of-raw target. The macOS ratio to the raw write is still printed and is kept in the table below.

Why the target changed: on `macos-14` the copy ceiling swung between 0.72 and 1.07 of the raw write across runners, and the sources were often not in the page cache, so 0.80 of raw could sit above what one read plus the required syncs can reach.

| run | commit | sampling | raw MB/s (`F_FULLFSYNC`) | import MB/s | import / raw | copy ceiling MB/s | copy ceiling / raw | **import / copy ceiling** |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| [39259267570](https://github.com/jay-the-maker/albumen/actions/runs/39259267570) | c401adb | single import | 557.637 | 302.937 | 0.5433 | 595.083 | 1.0672 | 0.509 |
| [39262347854](https://github.com/jay-the-maker/albumen/actions/runs/39262347854) | cf42e75 | single import | 706.932 | 381.610 | 0.5398 | 509.054 | 0.7201 | 0.750 |
| [39265120601](https://github.com/jay-the-maker/albumen/actions/runs/39265120601) | 60d6745 | 6 interleaved rounds | 552.450 | 328.938 | 0.5954 | 435.168 | 0.7877 | 0.756 |
| [#110 checks](https://github.com/jay-the-maker/albumen/pull/110/checks) | d37b6f2 | 6 interleaved rounds | 575.409 | 350.927 | 0.6099 | 488.676 | 0.8493 | 0.718 |

Every run reconciled 100 percent. In the interleaved runs, `vmtouch` found 0 to 67 percent of the source pages in the page cache before each macOS import, while both Ubuntu legs were at 100 percent. `macos-14` is at about 72 to 76 percent of its copy ceiling, so this target is still open there.

The macOS import is measured with the read-back coming from the device. Since [#124](https://github.com/jay-the-maker/albumen/pull/124), the destination write sets `F_NOCACHE`, so the independent SHA-256 read-back reads the stored bytes from the device instead of from cached pages. In #124's CI the read-back read went from about 2% to 18–19% of import time. The 90% target is unchanged and includes that cost. The copy ceiling has no read-back, as before.

The durability calls, both SHA-256 passes, the destination read-back, the raw sample, and the 100 percent reconciliation gate are unchanged.

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

The destination is a directory on the same runner. `copy_verify` reads each source once and updates SHA-256 as those bytes are written. It then reads the destination back and hashes that independent read. That read-back stays. The preserved mtime is set on the file and covered by the same `sync_all` as the bytes. The directory is synced after that, and the file is reported stored only after both have returned. Files are imported on a worker pool of `available_parallelism`, capped at 8, so hashing overlaps I/O. After the import, the harness removes that directory. It is a directory this process just created. The source directory stays. Each source is marked read-only before the import. Reconciliation is 100 percent only when `count_in` and `count_out` are both 1,000, `mismatched` is 0, and a second SHA-256 plus the length and mtime of every source still match the snapshot taken before `copy_verify`.

The raw write creates the same number of new files, of the same size, one file at a time, with the same durability calls: `File::sync_all` on the file and `dest::sync_dir` on the directory after each file. It does not hash and it does not preserve a source mtime. It is not parallel. 1 MB is 1,000,000 bytes. The ratio is import MB/s divided by that raw MB/s. The target is 0.80 or better. The job fails when reconciliation is short of 100 percent. A ratio below 0.800 is printed and is not a failure. The runs below are the measurements against that target.

```bash
cargo run --release --locked -p albumen-bench -- --throughput
```

The command prints JSON. `ALBUMEN_THROUGHPUT_FILES` and `ALBUMEN_THROUGHPUT_BYTES` override the size. A value below the CI default is refused unless `ALBUMEN_THROUGHPUT_ALLOW_SMALL=1`. CI does not set that variable.

The legs are `ubuntu-24.04`, `ubuntu-26.04`, `macos-14`, and `windows-2025`. Each leg prints `import-throughput` with its ratio and `target=0.800`. The job fails when reconciliation is short of 100 percent. It does not fail when the ratio is below 0.80. `windows-latest` is not a separate leg.

The corpus is 5 GB of sources and 5 GB of destination at the same time. CI sets `ALBUMEN_THROUGHPUT_DIR` to a directory under the workspace. A run that wrote the same corpus under `/tmp` on `ubuntu-26.04` stopped at `img-0332.jpg` with `Disk quota exceeded (os error 122)` and produced no ratio.

### Before

These runs imported one file at a time. `copy_verify` hashed the source while copying, hashed the destination again, fsynced the bytes, then fsynced the preserved mtime on its own. The raw write was the same sequential durable write used after the change.

Two back-to-back runs on this machine's ext4 disk, 1,000 files of 5,000,000 bytes, both reconciled 1,000 of 1,000 with `source_changed` 0:

| Run | Import MB/s | Raw MB/s | Ratio |
| --- | --- | --- | --- |
| 1 | 280.480 | 602.377 | 0.4656 |
| 2 | 279.289 | 737.715 | 0.3786 |

Import stayed near 280 MB/s. The raw write moved, and the ratio moved with it. Both ratios are below the 0.80 target.

The same sequential import on the pull-request runners, run `39236324899`:

| Leg | Import MB/s | Raw MB/s | Ratio | Reconciliation |
| --- | --- | --- | --- | --- |
| ubuntu-24.04 | 119.248 | 187.968 | 0.6344 | 100% |
| ubuntu-26.04 | — | — | — | stopped, disk quota on `/tmp` |
| macos-14 | 92.279 | 707.097 | 0.1305 | 100% |
| windows-2025 | 102.751 | 143.615 | 0.7155 | 100% |

### After

The import below hashes the source in the same pass as the write, keeps the destination read-back, runs files on the capped worker pool, and fsyncs the preserved mtime with the bytes. The raw write is unchanged: still one file at a time, with the same `sync_all` and `sync_dir`, and still without a hash. A ratio above 1 means the parallel import moved more bytes per second than that sequential raw write. The 0.80 target is unchanged. A ratio in this table is the measurement.

Two back-to-back runs on the same ext4 disk, 4 workers, 1,000 files of 5,000,000 bytes, both reconciled 1,000 of 1,000 with `source_changed` 0:

| Run | Import MB/s | Raw MB/s | Ratio |
| --- | --- | --- | --- |
| 1 | 842.915 | 874.803 | 0.9635 |
| 2 | 865.906 | 848.398 | 1.0206 |

Both are at or above the 0.80 target on this disk. The runner table is the per-leg measurement, from run `39237870279`. Every leg reconciled 100 percent. Workers were 4 on the Ubuntu and Windows runners and 3 on `macos-14`.

| Leg | Import MB/s | Raw MB/s | Ratio | Reconciliation |
| --- | --- | --- | --- | --- |
| ubuntu-24.04 | 276.535 | 417.350 | 0.6626 | 100% |
| ubuntu-26.04 | 288.874 | 388.815 | 0.7430 | 100% |
| macos-14 | 200.794 | 754.418 | 0.2662 | 100% |
| windows-2025 | 194.479 | 241.252 | 0.8061 | 100% |

`windows-2025` is at the 0.80 target (0.8061). The other three legs are below it. Import MB/s about doubled on each leg against the before table. On `ubuntu-24.04` the raw write also moved, from 187.968 MB/s to 417.350 MB/s, so the ratio moved from 0.6344 to 0.6626. On `macos-14` the raw write stayed near 700 MB/s and the ratio moved from 0.1305 to 0.2662. The 0.80 target is still the target. These ratios do not replace it.

Regression gate, separate from the 0.80 target: fail a leg when a later run is under 80% of the ratio recorded above. That would be 0.530, 0.594, 0.213, and 0.645 for the four legs. It is not a CI failure. The raw write on `ubuntu-24.04` moved by more than that between the before job and this one, so a gate on one sample would fail a later job when the raw write is faster and the import is not. Meeting that gate would not mean the leg met 0.80.

### Same durability primitive, and a median raw rate

`File::sync_all` is the import's file durability call, and the honest raw sample uses that same call plus `dest::sync_dir`. On Linux that is `fsync`. On macOS it is `F_FULLFSYNC`, not plain `fsync`. On Windows it is `FlushFileBuffers`. The macOS job also writes a plain `fsync` sample and prints `raw_fsync_mb_s` beside `raw_fullfsync_mb_s`. The ratio uses `F_FULLFSYNC` on that leg, because that is what the import uses. A plain `fsync` raw rate would make the ratio look worse without matching the import. On Linux `raw_fsync_mb_s` is the honest rate and `raw_fullfsync_mb_s` is absent, because the OS has no `F_FULLFSYNC`. On Windows both of those fields are absent and `raw_sync` is `FlushFileBuffers`.

The honest raw rate is the median of three samples before the import and three after it. The samples are deleted between passes. One cold or hot sample does not set the ratio by itself. The 0.80 target is unchanged.

The import records where its worker time went. The shares are sums across the worker pool, so they can add to more than the wall clock. A local ext4 run, 4 workers, 1,000 files of 5,000,000 bytes, reconciled 100 percent. The six honest raw samples were 664.424, 864.267, 775.770, 779.675, 832.661, and 776.422 MB/s. The median raw rate was 778.049 MB/s. Import was 861.589 MB/s. The ratio was 1.1074, with `raw_sync=fsync`.

| Phase | Share of worker time |
| --- | --- |
| file sync | 0.481 |
| read-back hash | 0.131 |
| hash while copying | 0.127 |
| write | 0.119 |
| source read | 0.081 |
| read-back read | 0.034 |
| directory sync | 0.027 |

The largest share is the file `fsync`. It stays. It is the same durability call the raw sample makes, and the file is not reported stored before it returns. The destination read-back stays too. The largest cost the raw sample does not pay is the two SHA-256 passes, 0.258 together. On x86 those already use the SHA extensions. On macOS the portable SHA-256 was what `sha2` used. `albumen-ingest` now enables the `sha2` `asm` feature, which selects the CPU SHA-256 on aarch64 when the processor has it, and leaves the x86 SHA-extension path in place. That is the attack on the hash share. The portable implementation remains the fallback when the CPU has no SHA-256 extension.

The per-leg ratios for this median and this hash path are filled from the pull-request job:

| Leg | Import MB/s | Raw MB/s | Raw sync | Ratio | Reconciliation |
| --- | --- | --- | --- | --- | --- |
| ubuntu-24.04 | pending | pending | fsync | pending | pending |
| ubuntu-26.04 | pending | pending | fsync | pending | pending |
| macos-14 | pending | pending | F_FULLFSYNC | pending | pending |
| windows-2025 | pending | pending | FlushFileBuffers | pending | pending |

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

## Search index at 380,000 items

`albumen-mcp` search reads a path index instead of walking the library on every query. The index holds each directory's sorted listing and modification time. Before each answer it stats the indexed directories and lists again only the ones whose time changed, so files added, renamed, or removed since the last run are in the next answer. A listing taken within 2 seconds of its directory's time is listed again on the next query, so a change inside one clock tick is still seen.

The index is saved outside the library: `--index-dir`, else `ALBUMEN_INDEX_DIR`, else `index` in the platform data directory. A directory inside the library is refused, and nothing is written inside the library. If the index directory cannot be written, search answers from the in-memory index. `albumen-mcp --build-index` builds or refreshes it and prints the time.

The `library scale` workflow (`scripts/library-scale.py`) reports the first index build as its own number, then times cold start, time to first answer, search p50 and p95 over 200 fixed queries, and peak memory against the saved index. Targets: time to first answer ≤ 1.5 s, search p95 ≤ 200 ms. `mcp/tests/search_index.rs` checks that index results equal a full walk, including files added, renamed, and removed between runs and while the server runs.

### Before the index

These are the walk-based numbers from before the index, when every search walked and statted the whole library. Same generator, 380,000 items, 200 fixed queries, seed 380000. Every leg, and a local Linux run, built the same library: `paths_sha256` `ec8ac920ee080c9d23c40c8a0a3110b3f255ef133109314336635852eda1886e`. Cold start here is spawn to `initialize` and `tools/list`, which needed no work before the index existed. Process start to first answer was one full walk.

| Leg | Run | Cold start | Process start to first answer | Search p50 | Search p95 | Peak memory |
| --- | --- | --- | --- | --- | --- | --- |
| `ubuntu-24.04` | [39244175785](https://github.com/jay-the-maker/albumen/actions/runs/39244175785) | 1.7 ms | 4,399.1 ms | 4,277.0 ms | 4,380.9 ms | 5.7 MiB |
| `ubuntu-24.04` | [39245400623](https://github.com/jay-the-maker/albumen/actions/runs/39245400623) | 2.1 ms | 5,313.7 ms | 5,268.2 ms | 5,459.8 ms | 5.6 MiB |
| `macos-14` | [39244175785](https://github.com/jay-the-maker/albumen/actions/runs/39244175785) | 21.4 ms | 12,897.5 ms | 10,151.1 ms | 13,228.3 ms | 6.0 MiB |
| `macos-14` | [39245400623](https://github.com/jay-the-maker/albumen/actions/runs/39245400623) | 28.8 ms | 18,185.9 ms | 10,867.1 ms | 14,131.4 ms | 5.9 MiB |
| `windows-2025` | [39245400623](https://github.com/jay-the-maker/albumen/actions/runs/39245400623/job/118035984598) | 9.1 ms | 27,114.3 ms | 26,280.8 ms | 27,645.0 ms | 6.4 MiB |

The 2,000-item smoke pass on `windows-2025` measured a search p95 of 192.2 ms with the walk. With the index, search p95 is 25 ms on `ubuntu-24.04`, 17 ms on `macos-14`, and 40 ms on `windows-2025`.

## First thumbnail at 380,000 items

`albumen-mcp` has a `thumbnail` tool. It reads one library JPEG through the read-only source root, decodes it in memory, scales it to fit 256 px (or `size`), and returns a new JPEG as base64. The original is not changed. Thumbnails are cached outside the library: `--thumbnail-dir`, else `ALBUMEN_THUMBNAIL_DIR`, else `thumbnails` in the platform data directory. A directory inside the library is refused. The cache key includes the original's size and modification time.

The `library scale` workflow places 32 copies of the CC0 fixture `fixtures/jpeg-gps/IMG_0001.jpg` (640 × 480) at fixed positions among the 380,000 stub items. A fresh `albumen-mcp` with the saved index and an empty thumbnail cache asks for one of them. The job reports the time from that request to the decoded thumbnail bytes and the time from process spawn to those bytes, both against the 300 ms library-scale budget, then the other 31 cold and all 32 from the cache. Report only.

Two thumbnail numbers are budgets, and both are team targets. The 1.0 s ceiling is for the full app's first painted thumbnail, from launch, and is measured once an app exists. The 300 ms budget is for the library-scale first thumbnail at 380,000 items: the `thumbnail` tool's bytes, measured in CI now. The 300 ms budget sits inside the 1.0 s ceiling and leaves room for the app's own startup and paint.

## Import CPU

The `import throughput` job reports the CPU of the 1,000-photo, 5 GB import, and its step summary shows a scorecard.

**CPU seconds per GB imported** is the metric. It is process CPU time (user plus system, all threads, measured around the timed import only, per round) divided by the gigabytes imported (10^9 bytes). It counts the copy, both SHA-256 passes, the fsyncs, and the read-back. A faster or slower runner disk does not change it. CPU seconds per 1,000 photos is the same number scaled by photo size: the job uses 5 MB photos, so 1 CPU second per GB is 5 CPU seconds per 1,000 photos.

Both numbers below are team targets, not requirements from Jay:

- **Scored target: ≤ 2.0 normalized CPU seconds per GB on every leg, median of 6 rounds** (10 CPU seconds per 1,000 five-megabyte photos). Status: **Partial** until every leg is at or under 2.0 normalized.
- **Regression gate: best of 6 rounds ≤ 3.0 normalized CPU seconds per GB on Linux and Windows, and ≤ 3.2 on macOS with the calibrated best round also ≤ 3.0 (see below); on Linux and Windows the median must also be ≤ 3.0.** On macOS the median and spread are printed and scored but not gated. The job fails above the gate. The gate reads its numbers from the same job and the same rounds it reports.

**macOS at 3.2 (temporary).** The macOS best-round gate is 3.2. Main went red on macos-14 at 7b00b53 (best 3.080) and 4cb9728 (best 3.370). Neither commit changes import code, and 33f85a7 scored best 1.721 on the same `macos-14-arm64` image 20260831.0302.1. Every phase moved together, including the pure SHA-256 phases, so this is runner variance and not code. Linux and Windows stay at 3.0 for the best round and the median. Getting macOS back to 3.0 is tracked in [#149](https://github.com/jay-the-maker/albumen/issues/149).

**macOS calibrated score (#149).** Across 12 macos-14 runs on the same image, the best round tracked the raw `F_FULLFSYNC` write rate measured in the same job (739 MB/s scored 1.721, 468 MB/s scored 3.370), while the in-memory SHA-256 rate barely moved. The job therefore also prints `calibrated` on the `import-cpu-gate` line and `cpu_seconds_per_gb_calibrated` in the JSON: the best round times `raw_mb_s / 650`, where 650 MB/s is the top of the leg's usual 600–650 band. On macOS the calibrated best round is gated at **3.0**, and the absolute best round stays gated at 3.2 as a backstop. Both must pass. The calibrated score is printed next to the absolute score and never replaces it. A fast disk scales the score up, so it cannot hide extra CPU. Linux and Windows are unchanged: absolute best and median at 3.0, no calibration. The 2.0 target is unchanged. F_FULLFSYNC, both hashes and the device read-back are unchanged.

**Normalization.** Runner CPUs differ, and SHA-256 is the work that varies most with them. The job therefore measures its own one-thread in-memory SHA-256 rate (`sha256_mb_s`) and rescales only the two hash phases, `hash` and `readback_hash`, to a 1,800 MB/s reference:

`normalized = raw − hash + hash × sha256_mb_s / 1800`

Here `hash` is those two phases' thread CPU per GB. Every other phase stays as measured. The job prints both `cpu_seconds_per_gb` (raw) and `cpu_seconds_per_gb_normalized`. The gate and the scored target use the normalized number. Without a usable rate, the raw number is used.

**Best and median of rounds.** The job runs 6 import rounds per leg. Each round's CPU is normalized with that round's own hash CPU. The gate reads the **best** round on every leg and the **median** round on Linux and Windows. macOS write CPU is bimodal across rounds on shared runners, so its median and spread (`cpu_seconds_per_gb_spread`, highest minus lowest) are printed and scored only. The scored target reads the median on every leg. The job prints every round as `cpu_rounds=raw/normalized/write_ms,...`, the best round as `cpu_seconds_per_gb_best`, the median as `cpu_seconds_per_gb_median`, and the all-rounds totals as `cpu_seconds_per_gb` (raw) and `cpu_seconds_per_gb_normalized`. A run with fewer than 6 rounds fails the gate. `ALBUMEN_THROUGHPUT_ORDER` (`raw-first`, the default, `import-first` or `alternate`) sets whether the raw sample runs before or after the import in each round. It is for measuring page-cache effects only; the gate is the same in every order.

**`cpu_percent`** stays in the report as context: the same CPU time divided by (import wall time × logical cores), times 100. It is no longer the target because it moves with disk speed: the same work on a disk twice as fast scores twice the percent. The earlier 10 percent target is printed beside it for comparison.

CPU time is `CLOCK_PROCESS_CPUTIME_ID` on Linux and macOS and `GetProcessTimes` on Windows; cores are `std::thread::available_parallelism`. The job also prints `phase_cpu_ms`, the thread CPU per copy phase.

### How the targets were set

CI runs on 2026-10-19, CPU seconds per GB per 5 GB round:

| leg | main ae1cb87 (run 39272133628) | #112 b453a10 (run 39274872606) | #112 cpu_percent, import MB/s |
|---|---|---|---|
| ubuntu-24.04 | 3.44 | 1.86 | 9.5%, 206 |
| ubuntu-26.04 | 3.28 | 2.79 | 14.3%, 201 |
| windows-2025 | 3.24 | 2.93 | 16.4%, 224 |
| macos-14 | 4.47 | 1.93 | 34.4%, 547 |

- **Gate.** The 3.0 gate is the slowest #112 leg (windows-2025, 2.93) rounded up to the next half second, so every leg passes once #112 is on main and a regression of a few tenths fails. Windows has the least headroom: a windows-2025 run of #116 on a slower runner CPU (in-memory SHA-256 1,581 MB/s against 2,061) measured 3.05. The gate stays at 3.0 on Windows too, and the Windows preflight work in #116 and after it is how that leg gains room.
- **Scored target.** The 2.0 target is what two legs already reach.
- **Floor.** The floor is the SHA-256 work alone: two passes per GB at each runner's one-thread in-memory rate (1,498 to 2,155 MB/s with the CPU SHA-256 instructions), about 0.9 to 1.3 CPU seconds per GB. That work stays.
- **What's left.** Windows preflight (4.2 CPU seconds per round) is the largest cost above the floor and the next cut.

**Why cpu_percent stopped being the target.**
- On ubuntu-24.04 the runner's disk was half as fast in the #112 run: the plain fsync copy in the same job fell from 417 to 163 MB/s. `cpu_percent` fell from 36% to 9.5%, well past the 46% drop in work per GB.
- On macos-14 the import ran 45% faster. Work per GB fell 57%, yet `cpu_percent` stayed at 34%.
