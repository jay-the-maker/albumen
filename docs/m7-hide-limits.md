# M7 hide refresh: known limits and follow-ups

The GUI keeps hide state per listed folder and per tombstone. Each request
stamps every listed folder; only a folder whose stamp changed (or was
stamped within 2 s of its mtime) is listed again, and in it only new or
changed tombstones are read. In an unchanged folder, only tombstones that
do not hide right now (`"hidden": false`, or unreadable) are stamped and
re-read on change; a tombstone that hides is not touched. A request where
nothing changed reads no tombstone (test
`an_unchanged_request_reads_no_tombstone`, counter `hide_tombstone_reads`
in `/api/found`) and serves the already filtered view. Each stamped entry
is its mtime plus a change key:

- Unix: the ctime (`MetadataExt::ctime`/`ctime_nsec`), which no user call
  can set back, so `rsync -t`/`-a` restoring a folder's old mtime is seen.
- Other OSes (Windows): a folder's entry count plus a hash of its
  tombstone names and sizes. Stable std does not expose the Windows change
  time.
- Off Unix only: for each stamped file (tombstones), its size plus a hash
  of its bytes up to 16 KiB, so a same-size rewrite with the mtime set
  back is seen. A larger or unreadable file gets a fresh key every
  request: it is always re-read (caching is off for it). Unix keeps the
  ctime alone and reads no tombstone bytes for the stamp.
- Anything modified within 2 s of a check is re-checked on the next
  request (same mtime tick).

## What waits for the next request, and what for the next listing

Per request (`/api/library` once the listing is ready, `/img`, `/video`):

- A new or changed tombstone beside a listed photo, in a folder that holds
  listed photos, and family-log hides: the photo drops out of
  `/api/library` and `/img`/`/video` return 404 on the next request.
- `/img` and `/video` read the photo's own tombstone on every request
  (`receiver_hidden`), so they 404 even before the listing is ready and for
  folders the stamp does not cover. An unreadable one is 404 and named in
  the quiet line. Outside a family library that, plus a folder that can't
  be listed, is the whole check (no per-listing work); a family library
  also asks the cached per-listing state.

Until the folder changes (a rename-replace in it, as albumen's own hide
and unhide do) or the next listing:

- **In-place rewrite of a tombstone that hides.** Rewriting a
  `"hidden": true` tombstone in place (same folder entry, no rename) to
  unhide, or to unreadable bytes, is not read on the next request: hiding
  ones are not re-stamped in an unchanged folder. Fail-safe direction (the
  photo stays hidden). `/api/found`'s quiet line still re-reads every
  watched tombstone per poll, so a broken one is named there at once.

Until the next listing:

- **Unhide.** The view no longer holds the photo. The request that sees
  the unhide starts a background re-list; the photo comes back when that
  listing is published, not on that request.
- **While a listing is in progress** (`status` not `ready`), `/api/library`
  pages are not filtered per request; a hide written during the listing is
  applied on the first request after it is ready.
- **New photos and new folders** appear only with the next listing; until
  then their folders are not stamped (their `/img` still checks the
  tombstone).

## Limits

1. **ctime on network or FUSE mounts.** SMB/CIFS, some NFS setups and
   FUSE filesystems may not keep a real ctime (it can mirror the mtime or
   be fixed). There, a hide synced in with the folder's old mtime restored
   is seen only if it changes the tombstone's bytes the GUI already
   watches. A brand-new tombstone in a folder whose mtime and ctime are
   both put back is missed until the next restart or rebuild.
   *Follow-up:* use the non-unix key (entry count + tombstone name/size
   hash) on Unix too, or when the ctime equals the mtime; measure the
   cost of one `read_dir` per listed folder per request at 380k files.
2. **No real rsync in CI yet.** The tests imitate `rsync -t` with
   `File::set_times`. *Follow-up:* add an ubuntu CI step that runs a real
   `rsync -a` of a tombstone (new, and same-size rewrite) into a running
   GUI's library and checks the next `/api/library`; run the same step on
   the PC run against a real library copy.
3. **Re-check cost (measured, box, 100 samples per level, p95 ms).**
   `m7_recheck_and_img_diagnostics`, `/api/library?offset=0&limit=1` with
   a folder touched before each request; before = b1b9363, after = the
   incremental cache.

   | files | tombstones | before unix | after unix | before forced non-unix | after forced non-unix |
   |---|---|---|---|---|---|
   | 50k | 0 | 22.4 | 17.3 | 30.5 | 24.0 |
   | 50k | 1k | 30.0 | 9.3 | 43.8 | 32.3 |
   | 50k | 10k | 86.3 | 27.8 | 127.5 | 71.4 |
   | 380k | 0 | 154.6 | 2.7 | 238.4 | 107.3 |
   | 380k | 1k | 140.2 | 97.7 | 229.3 | 185.1 |
   | 380k | 10k | 194.1 | 114.5 | 324.3 | 240.6 |

   After, unix p50: 11.9 / 7.9 / 22.4 (50k), 2.4 / 2.5 / 2.9 (380k).
   Steady state on Unix is ~2-3 ms at 380k. The p95s above 10 ms are the
   2 s same-tick window: the diagnostic writes the tombstones (every
   folder) right before sampling, so for 2 s every folder is listed again
   per request (and at 50k the whole level fits in that window). With the
   window cut to 50 ms (local experiment, not committed) Unix p95 is
   0.8 / 1.1 / 1.0 (50k) and 2.8 / 3.1 / 4.1 (380k). *Proposal (not
   applied):* a shorter same-tick window where the filesystem's ctime
   resolution is known fine (ext4/xfs/btrfs/tmpfs), 2 s elsewhere.
   Off Unix the folder key lists every folder per request (~24 ms at 50k,
   ~107 ms at 380k with no tombstones): the bar is not met there.
   *Proposal (not applied):* key folders and tombstones on the Windows
   ChangeTime (`GetFileInformationByHandleEx(FileBasicInfo)` via
   windows-sys), which no user call sets back, instead of listing and
   hashing; not compiled here (no Windows target on the box).
   Windows CI before (50k, p95): 91 / 205 / 1256 ms; after: not yet run.
4. **`/img` cost (measured, same runs, p95 ms).** Before unix 0.6 / 1.8 /
   14.2 (50k) and 0.6 / 1.7 / 12.5 (380k); before forced non-unix 13.5 /
   20.0 / 65.3 (50k) and 96.2 / 101.9 / 163.6 (380k). After, both keys,
   all levels: 0.2-0.6 (outside a family library `/img` reads only the
   photo's own tombstone and lists its folder). Windows CI before: 45 /
   114 / 959 ms; after: not yet run.
5. **Windows is covered by CI only** (`m7 hide tests (windows-2025)` in
   `.github/workflows/m6-shared-index.yml`). The non-unix key was checked
   on Linux by forcing that cfg path locally; it has not run on Windows
   here.

## XO 02:30Z follow-up (same-tick window, Windows ChangeTime, /api/found cache)

- **Same-tick window by filesystem.** 50 ms only when `statfs` names a
  filesystem with a fine ctime (Linux f_type ext4, xfs, btrfs, tmpfs,
  f2fs, zfs; macOS `apfs`). Everything else gets 2 s: unknown, NFS,
  SMB/CIFS, FUSE, FAT/exFAT, HFS/HFS+, overlayfs, ntfs3, and Windows
  (no statfs). Chosen once per listing for the library root; a different
  filesystem mounted below the root uses the root's window (limit).
  `/api/found` shows `hide_same_tick_ms` and `hide_fs`. Test hook:
  `ALBUMEN_TEST_FS_MAGIC` (a Linux f_type) forces a type on any platform.
- **Windows change key = ChangeTime** (`GetFileInformationByHandleEx
  (FileBasicInfo)`), for folders and tombstones; no listing or hashing.
  If it can't be read the entry gets no stamp (re-read every request,
  fail closed) and `hide_key_failures` counts it: no silent fallback to
  another key. `/api/found` shows `hide_key` and `hide_changetime_keys`;
  `the_change_key_is_the_platform_key_with_no_fallback` asserts them.
  Not compiled on the box (no Windows target); proved by the m7-windows job.
- **The portable key** (old non-unix key) is now compiled everywhere and
  forced with `ALBUMEN_TEST_HIDE_KEY=portable` (replaces editing the cfg).
- **`/api/found`** stamps every watched tombstone each poll and re-reads
  only changed, new or same-tick ones; the family log is still read every
  poll. `a_corrupted_tombstone_is_in_the_note_within_one_poll`.
- **Measured (box, release, 100 samples each; box timing is not evidence),
  re-check p95 ms, 0 / 1k / 10k tombstones.** "sync 2 s" = samples within
  2 s of the last tombstone write; "steady" = 100 samples after that.

  | files | key | sync 2 s | steady |
  |---|---|---|---|
  | 50k | ctime | 0.9 / 7.7 / 1.1 | 0.9 / 0.9 / 1.0 |
  | 380k | ctime | 2.8 / 2.8 / 3.2 | 2.8 / 2.8 / 3.2 |
  | 50k | portable | 13.4 / 15.1 / 23.7 | 13.4 / 17.3 / 24.7 |
  | 380k | portable | 100.7 / 303.8 / 324.0 | 119.7 / 113.9 / 134.3 |

  The portable key still lists every folder per request, so it misses
  the 10 ms bar; on Windows it is no longer used (ChangeTime), but
  Windows keeps the 2 s window, so its first-2 s figures will be higher
  than Linux's. Windows not yet measured.

## XO 02:44Z follow-up

- **ext2/3/4 coarse.** 0xEF53 is shared by ext2, ext3 and ext4, and ext2/3 (or ext4 with 128-byte inodes) keep whole seconds, so 0xEF53 now gets the 2 s window (`hide_fs` = `ext2/3/4`). No probe was used: an inode cached in memory can show nanoseconds that are cut off on disk, so reading ctime nanoseconds isn't a robust test. The cost is only in the first 2 s after a change.
- **Test hook.** `ALBUMEN_TEST_FS_MAGIC` works only in debug builds or with the `test-hooks` cargo feature; a plain release build ignores it. CI's M7 commands (and the scale step, via `M6_FEATURES` in `.github/m6-one.sh`) pass `--features test-hooks` to every M7 command, so one release build serves every step and the forced-fallback test still runs under `--release`. Without the feature, the release test is `ignore`d (20 passed, 1 ignored), so CI's `0 ignored` guard catches a missing feature.
- **Maxima (box, release; box timing isn't evidence).** At 380k files, the largest re-check is always sample k=0, the first request after the tombstone batch is written: 125.8 ms at 1k and 187.9 ms at 10k (forced fine window); 128.2 ms and 202.5 ms with the ext 2 s window. The cause is the one-off rebuild after the batch: re-listing the changed folders and reading the new tombstones. The files were just written, so they're in page cache, and Rust has no GC. Under the 2 s window, every request within 2 s of the writes repeats that work (sync2s p95 93.1 / 202.5 ms); the steady state is unchanged (p95 2.6–3.7 ms, max ≤ 4.1 ms). New `M7 RECHECK_SLOWEST` lines print the three slowest samples with their index and their time after the writes.

## XO 02:53Z: post-sync rebuild (bar not relaxed)

Bar: re-check p95 <= 10 ms and max <= 100 ms over ALL samples (first 2 s after a sync included); ext 2 s window kept.

**Cause (old code, new counters `hide_recheck_reads` / `hide_dirs_listed`):** k=0 lists all 50 dirs (380k entries) and reads every new tombstone (144.2 ms / 1000 reads at 1k; 200.4 ms / 9000 reads at 10k); then every request inside the 2 s window re-lists all 50 dirs (0 reads) at ~100-175 ms each, because their stamps are untrusted.

**Fix:** (1) `stamp_window`: the trust time is max(mtime, ctime); on ext a ctime with a non-zero sub-second part proves ns stamping, so it uses the 50 ms fine window; a whole-second ctime keeps 2 s. An evicted 128-byte inode reloads without ns, changing the key and forcing a re-list (fail-closed). (2) Dirs needing a re-list are listed on up to 8 scoped threads (serial when <= 1). Same-size swap, rsync mtime restore, corrupt tombstone within one poll, forced fallback and new-tombstone-in-folder tests stay green.

**Results (box, release, no build running; box timing is not evidence):** 380k unix p95/max 0: 2.7/2.9 (rerun 3.1/3.2); 1k: 3.1/52.4 (3.3/50.9); 10k: 3.2/93.0 (3.8/71.4). 50k unix: 0.8/11.6, 3.1/11.0, 1.4/38.9. Forced portable key MISSES: 380k 104.4/127.0, 119.4/152.4, 134.3/209.6; 50k 18.4/20.9, 22.3/30.5, 48.9/62.1 (portable hashes every dir listing each request).

CI: the Linux diagnostic step now fails if any `M7 RECHECK_DIAG` line has p95 > 10 or max > 100.

## XO 03:09Z / 03:12Z (Windows spikes, change-key line, bars)

- **Windows ChangeTime is sub-second.** `stamp_window` takes the key kind. A Windows ChangeTime (100 ns ticks since 1601) that has a non-zero sub-second part gets the fine 50 ms window, and its `since` is max(mtime, ChangeTime). A whole-second ChangeTime (from a coarse SMB server) keeps 2 s, and so does one from before 1970. On a26fd4f (run 41163090130), the Windows 50k spikes after the k=0 sample were folder re-lists with reads=0 and listed=26: 139.3 ms at 1k, then 318.1 ms and 252.3 ms at 10k. Those folders were untrusted only because of the 2 s window.
- **One open per tombstone on Windows.** One handle is opened (read, without following a reparse point). It takes the stamp (LastWriteTime + ChangeTime from `FileBasicInfo`) before reading the bytes, which are parsed by `albumen_ingest::hides::parse_receiver_tombstone`. Before this, each tombstone took a stat, an attributes open and a read open. A directory, a reparse point or an open failure falls back to the old path, and a stamp that can't be read is left untrusted. Either way it fails closed.
- **A post-sync rebuild keeps each folder's state.** A new generation carries over each folder's stamp, trust and tombstones by folder name. A trusted folder with an unchanged stamp is not listed again, and the hidden set is still rebuilt for the new view. Before this, every generation change listed every folder and re-read every tombstone. Re-listing is still spread over up to 8 threads.
- **Whole-second ctime on ext.** The test `a_whole_second_ctime_on_ext_keeps_the_2s_window` uses `ALBUMEN_TEST_COARSE_CTIME=1`, a test hook that drops the ctime nanoseconds. It proves that a whole-second ctime on 0xEF53 keeps re-reading through 2 s and stops after it. With fine ctimes as the control, it doesn't re-read.
- **Change-key line.** On Windows the line was missing because the "M7 hide tests" step runs without `--nocapture`. The test ran and passed, but its println output was captured. A new Windows step runs only that test with `--nocapture` and requires `M7 change key: key=changetime changetime_keys>0 key_failures=0`.
- **Bars.** `.github/m7-recheck-bar.sh` checks every `M7 RECHECK_DIAG files=` line. Each line must have numeric `recheck_p95_ms` and `recheck_max_ms`; a missing field fails. It requires p95 <= 10 ms and max <= 100 ms. The Linux 380k step and the Windows 50k step both use it.
- **Known gap.** At k=0 the 9000 new tombstones in the 10k level have to be read; they are new, so this can't be avoided. That cost was 378.3 ms on Windows a26fd4f, and only CI can show whether one open per tombstone brings it under 100 ms. If it doesn't, the Windows 10k bar fails, and that is the intended blocking result.

## XO 03:22Z: background confirmation, folder-list counts, NTFS writer-open

Ruling (Product Lead, not Jay): max <= 100 ms at k=0 after a Windows sync
with 10,000 hides, not relaxed. a26fd4f's Windows 50k run measured k=0
378.3 ms (9000 reads, 26 folders listed) and 250-320 ms for listing 26
folders alone, so the request path no longer lists or reads changed folders
itself after the first answer:

- Each check stamps every folder (one stat each). An unchanged trusted folder
  is handled inline as before. Any other folder is handed to background
  workers (up to 8 threads) that list it and read its tombstones.
- Once primed the request does not wait for that work (XO 04:08Z; the
  40 ms INLINE_BUDGET is gone), except up to 40 ms (BLINK_BUDGET) when a
  photo on screen would otherwise be withheld. A folder not confirmed is
  *pending*: a photo shown before stays shown only if its own tombstone is
  absent or unchanged since a trusted show (one stat); every other photo
  in it is withheld (A8, fail closed) until a background applier puts the
  worker's result in place. A new hide is therefore hidden on the first
  request either way.
- A result is applied by folder name across a new listing generation; its
  stamp was taken before the listing, so a folder that changed again is
  simply listed again. A photo only withheld comes back by restoring the
  unfiltered view (no re-list); a real unhide still re-lists as before.
- The first check of a server (not primed) still runs in full inline.
- `/api/found` adds `hide_pending_dirs`, `hide_withheld_dirs` (folders that
  went through the queue), `hide_withheld_max_ms`, `hide_withheld_last_ms`
  (hand-over to applied), and `hide_writer_untrusted`.
- Photos come back on the next check after the worker finishes: the UI's
  polling is what applies them (no push).
- Within a folder's same-tick window an untrusted folder is re-queued on each
  check, so its photos can be withheld again briefly (fail closed, by design).

Box numbers (50k files, 50 folders, release, direction only, not evidence;
withheld = from the end of the tombstone writes until no folder was pending):

| key | hides | withheld_ms | k=0 request ms | max over 100 | p95 |
|---|---|---|---|---|---|
| ctime | 1,000 | 16.7 | 10.9 | 10.9 | 3.0 |
| ctime | 10,000 | 69.3 | 22.4 | 22.4 | 5.8 |
| portable (forced) | 1,000 | 29.6 | 22.1 | 22.3 | 18.5 |
| portable (forced) | 10,000 | 120.8 | 51.0 | 51.0 | 43.0 |

Forced portable misses the p95 <= 10 ms bar on the box at every level
(steady 15-16 ms, re-listing 26-50 folders per check): it is a test mode and
CI does not run the diag with it.

NTFS times with a writer open: MS Learn "File Times" says the only guarantee
is that a time is right once the handle that changed the file is closed, and
the last write time is not fully updated until every writing handle closes;
MS Open Specs note NTFS set the write time at close up to Server 2016 and at
each write from Server 2019. So a stamp may lag while a writer is open. The
design fails closed: (1) the Windows tombstone read keeps its stamp only if
ChangeTime, LastWriteTime and size (FileStandardInfo) are identical before
and after the read on the same handle and the bytes read are the whole file,
else the tombstone is untrusted and read again next check; (2) the bytes are
read on every untrusted or changed stamp, and the writer's close moves the
times, so a stamp taken while it was open never matches again. Residual: a
writer that rewrites a tombstone in place at the same size and never closes
it, on a filesystem that lags ChangeTime, is not seen until it closes. The
Windows-only unit test `gui::tests::ntfs_changetime_while_a_writer_is_open`
(CI Windows step) prints the before/open/after times and asserts the bytes
are seen at once and the stamp moves at close.

Folder listing across a sync (`only_changed_folders_are_listed_across_a_sync`,
box ctime): first request after a hide in one of three folders lists 1
folder; after its window 1 more; then 0 over 3 checks; a recreated folder
lists 1 and its new hide applies on the first request.

## XO 04:08Z: Windows k=1/k=2 stalls, withheld time, no-flicker test

Root cause of the Windows 10k miss (job 124431323931; note
1zgYReyhVIS9D5v4HnyohSvlLIGRwy96n): the 40 ms budget capped only the wait
for workers. The request still rebuilt the 50k filtered view and the
hidden/withheld set inline, and a landed folder was applied only by the
next request, so k=1 and k=2 each paid a full rebuild while 8 workers
competed for the cores (246.9 ms at k=1, 109.5 ms at k=2). Withheld time
was high because nothing applied results between requests and the page
polled every 10 s. The same cause shows on Linux 380k (job 124431323728:
max 150.5 ms, withheld 523.9 ms at k=2). The split on Windows is from the
code and the log, not measured on Windows.

Changes (branch m7-noflicker from d96e701; the tree difference between
d96e701 and 6c8d50c is one file mode, accepted by the Product Lead):

- No inline wait once primed (BLINK_BUDGET 40 ms only when a shown photo
  would be withheld); a single-flight, coalescing background applier puts
  each finished folder and the rebuilt view in place.
- Workers use available cores minus one (1..8), so requests keep a core.
- A worker panic is caught, logged (`hide: worker panicked; retrying
  <dir> (attempt N)`), counted (`hide_worker_panics`) and re-queued up to
  3 times; after that the folder is unlisted (its photos hidden).
- Bug found by the no-flicker test and fixed: a photo hidden, then
  withheld while its folder was pending, then shown when the folder landed
  was not treated as an unhide (it was only "released"), so the gui never
  listed the library again; the photo and any new photos stayed missing
  until another unhide. `hard`/`hard_w` now track it.
- The page polls `/api/found` every 200 ms while a folder is pending.
- CI: withheld and UI p95/max <= 1 s are asserted by
  `.github/m7-recheck-bar.sh` (no `|| true`); a YAML error in the 380k
  step name (`bar: `) is fixed.

No-flicker acceptance (PL doc 1Ak9gqDUQp4BfibMVlw4fd4qLw8CKUxPH section
2): `m7_no_flicker_ctime` (Linux) / `m7_no_flicker_changetime` (Windows),
`_coarse_window`, `_worker_error`, `_worker_panic`. Folder d00 is held
pending by `ALBUMEN_TEST_RELIST_GATE=d00` while `<library>/.albumen-test-gate`
exists. 5,000 photos in 50 folders, 250 hides, 50 show tombstones; the
sync adds 20 photos and a new hide h in d00 and a non-photo file, and
removes one hide in d01 (deviation: the gui lists new photos only after
an unhide or a scan, so the unhide is what brings them in). At >= 20
requests while gated: no photo of V0 except h disappears, h never shows,
no new photo shows, no H0 photo shows, `hide_pending_dirs >= 1` (a gate
not honoured fails). After the release the new photos show within 1 s,
the set is exact and stable for 5 requests. Then 20 syncs measure the
withheld time (p95 and max <= 1 s, asserted) and request time (p95 <= 10,
max <= 100 ms, asserted, the whole `/api/library` call). Variants:
coarse forces the 2 s same-tick window (Linux: ext + whole-second ctime
hooks; Windows: already 2 s) and does not wait it out; worker_error makes
h's tombstone unreadable (`ALBUMEN_TEST_TOMB_ERROR`), asserted in
`hide_note` and stderr; worker_panic panics the gated re-list
(`ALBUMEN_TEST_RELIST_PANIC_AFTER_GATE=1`) and asserts the log line and
`hide_worker_panics >= 1`. Deviation: the PL's five variants are ctime,
ChangeTime, coarse, error, panic; each platform runs four (the key variant
is compiled for its own platform), so CI requires "4 passed; 0 failed;
0 ignored" and one `M7 NO_FLICKER` line per variant on each.

Box (Linux ctime, not CI evidence): all 4 pass; disappearances=0,
h_shown=0, release-to-new ~56 ms, withheld p95 ~420-430 ms, max <= 633 ms,
request p95 7.6-9.8 ms, max <= 49 ms. 50k diag (3 runs): 10k re-check
p95 11.6 / 9.2 / 7.6 ms, max 41.2 / 34.5 / 34.1 ms; withheld p95 <= 15.2
ms, max <= 22.4 ms; UI p95/max <= 24.3/28.0 ms. One of the three runs
missed p95 <= 10 ms at 10k on the box; d96e701 on the box gave 5.0 ms
p95 / 22.2 ms max, so the p95 is not better than d96e701 here and the
max is. Windows is unverified until CI runs.

Gaps: the UI still refetches the full library on a version change (380k);
PENDING_LIMIT re-dispatch has no dedicated test; deny-write is not
verified on Windows; GUI DOM harness open.

## XO 04:41Z (branch m7-r2, on 0e54af1)

Box numbers are Linux on the shared box, not CI evidence.

1. Synced-in photos: before this change a new photo reached the view only by a
   re-walk, which only an unhide or a scan started. A confirmed listing that holds
   a photo not in the base view (and not hidden by its own tombstone) now starts
   the same re-walk (`hide_grew`). Test `m7_synced_in_photos_show_without_unhide_or_scan`
   (box: shown in 202 ms).
2. Poll: `GET /api/hide-status` returns `{pending, version, generation, found}`
   (54-57 bytes) and clones nothing. The page polls it every 200 ms while folders
   are pending and every 10 s otherwise, and refetches `/api/library` only when
   the version or generation changes. Box 380k, 0 tombstones: p50 0.07 ms,
   p95 0.37 ms; after a 1k-tombstone sync p50 7.9 ms, p95 24 ms, max 26 ms
   (`/api/found` at the same point: p50 36 ms, p95 111 ms, 53 KB).
3. Tests `m7_pending_limit_sends_a_hung_folder_again` and
   `m7_exhausted_panics_keep_shown_photos_and_back_off`.
4. An exhausted listing no longer marks the folder unlisted: it stays pending
   with its per-photo keep set and is listed again after 1, 2, 4 ... 60 s.
5. `keep_shown` lists each folder once instead of one stat per shown photo
   (Windows CI on 0e54af1: keep_max 21/101/148 ms at 50k and 0/1k/10k
   tombstones). Keep and set-build times are asserted <= 50 ms up to 50k files
   and <= 1 s above that. They still grow with the photos of the folders going pending:
   box 380k after a 1k-tombstone sync, keep 244 ms and build 172 ms.
6. `no_flicker` checks the order while pending (`listed_raw`; `listed` sorts).
7. Test `m7_unhide_while_pending_shows_the_photo`.

Gaps: there is no diff endpoint (a change refetches the whole library), and a
new folder outside a known one is not seen until a re-walk. At 380k after a
1k-tombstone sync the box measured a server withheld max of 1271 ms (one run)
against the 1 s bar.
