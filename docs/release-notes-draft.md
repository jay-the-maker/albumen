# Draft release notes: next version after v0.1.1

Status: **draft**. This is not a release, and there is no tag. Jay picks the version and publishes it. Every item below is already on `main` (up to `4dad880`). Work still in the merge train is listed at the end and does not ship until it merges. The full list is in [CHANGELOG.md](../CHANGELOG.md).

## Highlights

- **Family libraries.** Back a phone up into its own family namespace (#83). Use one-step export of every member's originals (#81), additive family export updates (#154) and `albumen family sync`, which merges two live libraries without removing anything (#165). Family tombstones keep every original (#155).
- **Deletes never propagate.** Tests prove that a delete, hide, revoke or tombstone on one copy never reaches the other copy, local or S3 (#137, #148, #157, #158). Receiver hide is dry-run first and reversible (#202).
- **Sources stay read-only.** Writes need an explicit apply (#48). Scan and export take a read-only source (#66), and the scan ledger stays off the source volume (#201).
- **Phones.** Android camera-roll backup over verified WebDAV (#59), the iOS host and background upload extension (#62), and a pairing QR for `albumen-receive` (#89).
- **Speed.** The import pipeline reads, hashes, writes and reads back on separate threads per lane (#110). Search uses a saved path index, tested at 380,000 items (#103).
- **Shell completions.** `albumen completions <shell>` (#75).
- **Install.** Release archives for Linux, Windows and macOS, installable without Rust (#65, #88).

## Upgrade notes

- The scan ledger now lives under `ALBUMEN_HOME`, or wherever `--ledger` points, never beside the source (#201).
- Windows: a failed directory flush is now an error (#144).

## Coming next (in the merge train, not in this release)

The headline items are `verify-manifest --proof`, exit codes 0–4 with a `next:` line, `takeout-fidelity`, `import-card`, and rclone export targets. Export dry runs and `--apply` will list every conflict and exit 3 without writing anything. See "Not yet released" in CHANGELOG.md and the train in `docs/merge-queue.md`.
