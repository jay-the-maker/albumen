# M7 hide refresh: known limits and follow-ups

The GUI re-reads hide files (tombstones) only when a stamp of the listed
folders changes. Each stamped entry is its mtime plus a change key:

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
- `/img` and `/video` also read the photo's own tombstone on every request
  (`receiver_hidden`), so they 404 even before the listing is ready and for
  folders the stamp does not cover. An unreadable one is 404 and named in
  the quiet line.

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
3. **Hashing cost off Unix, not yet measured.** Each stamp check (every
   `/api/library`, `/img`, `/video` request once ready) reads every
   watched tombstone in full, up to 16 KiB each: about tombstones x
   (size) bytes per request, e.g. 10k tombstones of ~60 B is ~0.6 MB, plus
   one `read_dir` per listed folder. *Follow-up:* the Windows job prints
   `M7 RECHECK_DIAG` at 50k files with 0, 1k and 10k tombstones; measure
   there before trusting it at 380k.
4. **Windows is covered by CI only** (`m7 hide tests (windows-2025)` in
   `.github/workflows/m6-shared-index.yml`). The non-unix key was checked
   on Linux by forcing that cfg path locally; it has not run on Windows
   here.
