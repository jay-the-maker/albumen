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
