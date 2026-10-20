# XO-52: real-device soak plan for "sync is not backup" (#166)

Status: **plan only. Nothing has run.** Jay runs every step on his own devices once he OKs it. No agent touches Jay's Mac, phones or RAID.

## What it proves

Two real copies of a library, with sync and removals exercised over a soak period:
- neither copy loses or changes any existing file;
- every removal stays a signed record (family hide or tombstone) or a copy-local `.albumen-tombstone`;
- no original is unlinked.

This is the "Done when" condition in #166.

## Ground rules

- **Read-only sources.** The real photo sources (Photos library, phone camera rolls, the RAID's existing folders) are only read: `albumen scan` opens nothing for write, and the importers open sources read-only. Nothing is written inside a source tree.
- **Fresh copies.** Copy A and copy B are new, empty directories that Albumen creates. Copy B goes on the RAID in a new folder, `albumen-soak-B/`. No existing RAID folder is a destination.
- **Out-of-tree logs.** Manifests and logs live in `~/albumen-soak/logs/`, outside every library and source.
- **Plan before apply.** Every command runs once as a dry run (without `--apply`) first, and Jay reads the plan before running it with `--apply`.
- **Checksums.** Verification is by SHA-256 throughout: `copy_verify` on every write, then `albumen verify-manifest` after every step.

## Setup (day 0)

1. Pick a subset, for example one year, about 5–20 GB, with Live Photos, RAW and video. Record the source path in `logs/source.txt`.
2. Baseline the source:
   `albumen scan <source> --manifest ~/albumen-soak/logs/source-day0.jsonl`
3. Build copy A:
   `albumen import-icloud --from <source> --to ~/albumen-soak/A --report logs/import-A.jsonl` (dry run), then the same with `--apply`. The command exits non-zero unless the count in equals the count out and every SHA-256 matches.
4. Export to copy B:
   `albumen export --from ~/albumen-soak/A --to /Volumes/<RAID>/albumen-soak-B`
   `albumen verify-manifest --manifest /Volumes/<RAID>/albumen-soak-B/ALBUMEN-MANIFEST.txt --against /Volumes/<RAID>/albumen-soak-B`
5. Snapshot both copies: run `albumen scan` on A and on B, writing `logs/A-day0.jsonl` and `logs/B-day0.jsonl`. These are the reference manifests.
6. Optional: one phone backup into A through the receiver, then step 4 again.

## Soak (days 1–7, or as long as Jay chooses)

Each day:

1. **New photos.** Add the day's new photos to A with the same importer (dry run, then `--apply`).
2. **Removals.**
   - Hide 2–3 photos in A through the receiver library hide, which writes a `.albumen-tombstone`.
   - Run `albumen family hide` on 1 photo.
   - Run `albumen family tombstone` on 1 photo.
   - Log every target in `logs/removals.txt`.
3. **Carry the changes to B.**
   - Run `albumen family sync --from ~/albumen-soak/A --to /Volumes/<RAID>/albumen-soak-B` as a dry run and check that it reports `add` only.
   - Run it again with `--apply`. It must print `removed: 0`.
   - Also run `albumen export` again from A to B; it is idempotent.
4. **Verify.**
   - Run `albumen verify-manifest` on B against its manifest.
   - Run `albumen scan` on A and B, writing `logs/A-dayN.jsonl` and `logs/B-dayN.jsonl`.
5. **Diff against day 0.** Every line in `A-day0` and `B-day0` must appear unchanged (path, size, mtime, SHA-256) in the day-N scans. New lines are only allowed for added photos, sidecars, signed records and tombstones.
6. **Re-check the source.** `albumen scan <source>` compared with `source-day0` must show no change and no `drift`, except for photos Jay added to the source himself.

## Success criteria

All of these must hold on every day of the soak:

- Every `verify-manifest` passes, with 0 missing, 0 extra unexplained and 0 different.
- No original or sidecar from any earlier day disappears from A or B, or changes size, mtime or SHA-256.
- Every entry in `removals.txt` exists as a `.albumen-tombstone` (on that copy only) or as a signed hide or tombstone record, and its original is still present on both copies.
- Every `family sync --apply` prints `removed: 0`.
- The source scans match day 0, so the sources were untouched.

Any failure stops the soak. Jay keeps the logs and opens an issue linked to #166.

## Rollback

- **Sources:** nothing to roll back. They are only read, and the source scan proves it.
- **Copies:** A and B are new soak directories. Rolling back means deleting `~/albumen-soak/A` and `/Volumes/<RAID>/albumen-soak-B`. Jay does that by hand, and only after the logs are saved. No Albumen command deletes anything.
- **Keys:** delete any family key made for the soak, after the copies are removed.
- **Phones:** delete the receiver pairing for the soak on the phone. The camera roll is not changed.
- **On a failure:** stop, keep A, B and the logs exactly as they are, and attach the manifests and the diff to the issue.

## Report back on #166

Post the dates, file counts and total size, the counts of removals by kind, the pass or fail of each daily `verify-manifest`, and the day-0-to-final diff summary for A, B and the source.
