# M7 hide refresh: known limits and follow-ups

The GUI re-reads hide files (tombstones) only when a stamp of the listed
folders changes. Each stamped entry is its mtime plus a change key:

- Unix: the ctime (`MetadataExt::ctime`/`ctime_nsec`), which no user call
  can set back, so `rsync -t`/`-a` restoring a folder's old mtime is seen.
- Other OSes (Windows): a folder's entry count plus a hash of its
  tombstone names and sizes. Stable std does not expose the Windows change
  time.
- Every OS: for each stamped file (tombstones), a hash of its bytes up to
  16 KiB. A larger or unreadable file gets a fresh key every request, so
  its folder is always re-read (fail closed).
- Anything modified within 2 s of a check is re-checked on the next
  request (same mtime tick).

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
3. **Windows is covered by CI only** (`m7 hide tests (windows-2025)` in
   `.github/workflows/m6-shared-index.yml`). The non-unix key was checked
   on Linux by forcing that cfg path locally; it has not run on Windows
   here.
