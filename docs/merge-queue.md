# Merge queue

Snapshot taken 2026-10-29 at about 1:50 AM PDT against `main` at `4dad880`. Jay merges, in this order. Nothing here is merged, force-pushed, or deleted.

How each column was read:

- **Head** and **ahead/behind**: `git rev-list --left-right --count origin/<base>...origin/<branch>` after `git fetch`. "behind/ahead" means commits on the base that the branch lacks, then commits the branch adds.
- **CI**: GitHub check runs for the branch head (`GET /repos/jay-the-maker/albumen/commits/<branch>/check-runs`). Every branch returned `total_count: 0`. `ci.yml` runs only on pushes to `main` and on pull requests, and no PRs are open, so CI has not run on any of these heads. Each PR will get its checks when Jay opens it.
- **Conflicts**: `git merge-tree --write-tree origin/<a> origin/<b>` for every pair that is not stacked.

Order: docs first, then tests and CI, then MCP and mobile, then export and CLI behaviour, then importers.

| # | Branch | Head | Base | Behind/ahead vs base | Behind/ahead vs main | Depends on | CI | Risk | Files |
|---|---|---|---|---|---|---|---|---|---|
| 0 | merge-queue | this commit | main | 0/1 | 0/1 | none | none (no PR) | Docs only; the snapshot goes stale as branches merge. | `docs/merge-queue.md` |
| 1 | issue-38-ci-doc-windows-2025 | d9cdbf0 | main | 0/1 | 0/1 | none | none (no PR) | One line in docs/ci.md. Low. | 1: `docs/ci.md` |
| 2 | issue-47-readme-try-it | 3499670 | main | 0/1 | 0/1 | none | none (no PR) | README only. Conflicts with exit-codes in README.md. | 1: `README.md` |
| 3 | readme-quickstart-timed | 250e2c4 | issue-47-readme-try-it | 0/2 | 0/3 | 2; describe after 14 lands | none (no PR) | README only. Conflicts with exit-codes in README.md. | 1: `README.md` |
| 4 | compare-doc | 5069720 | readme-quickstart-timed | 0/3 | 0/6 | 3 | none (no PR) | Competitor claims age; every one is sourced and dated 2026-10-29. Conflicts with exit-codes in README.md. | 3: `.github/workflows/link-check.yml`, `README.md`, `docs/compare.md` |
| 5 | issue-45-live-photo-edge-tests | 97b53f0 | main | 0/1 | 0/1 | none | none (no PR) | Test-only, in ingest/src/discover.rs. Low. | 1: `ingest/src/discover.rs` |
| 6 | issue-46-receive-frontend-tests | 1134a01 | main | 0/1 | 0/1 | none | none (no PR) | Test-only, in receive/src/roundtrip.rs. Low. | 1: `receive/src/roundtrip.rs` |
| 7 | issue-138-dir-sync-scale | 7c531b8 | main | 0/1 | 0/1 | none | none (no PR) | New test; partial fix for #138, Windows CPU target not measured. | 1: `ingest/tests/dir_sync_scale.rs` |
| 8 | scan-export-scale | 5005ece | issue-138-dir-sync-scale | 0/2 | 0/3 | 7 | none (no PR) | New weekly CI job. Baseline is a placeholder, so the 20 percent check is not enforced until Jay records one. | 4: `.github/workflows/scan-export-scale.yml`, `bench/baseline.json`, `bench/src/main.rs`, `scripts/scan-export-scale.py` |
| 9 | issue-42-sidecar-names | fb30349 | main | 0/3 | 0/3 | Jay commits 4 fixture images first | none (no PR) | Blocked on fixture images Jay must add; tests fail without them. | 9: `fixtures/README.md`, `fixtures/SHA256SUMS`, `fixtures/generate.sh`, `fixtures/manifest.json`, … |
| 10 | issue-mcp-stdio-ndjson | 5ccb092 | main | 0/2 | 0/2 | none | none (no PR) | Changes MCP stdio framing. P1: standard MCP clients cannot connect without it. | 5: `mcp/src/io.rs`, `mcp/tests/client.rs`, `mcp/tests/source_readonly.rs`, `mcp/tests/stdio_framing.rs`, … |
| 11 | mcp-real-clients | 5f64151 | issue-mcp-stdio-ndjson | 0/1 | 0/3 | 10 | none (no PR) | Docs and smoke scripts. Low. | 3: `docs/mcp-clients.md`, `scripts/mcp-sdk-smoke.mjs`, `scripts/mcp-smoke.py` |
| 12 | mcp-thumbnail-image-block | 6159a07 | mcp-real-clients | 0/2 | 0/5 | 11 | none (no PR) | Changes the thumbnail tool's response shape. | 6: `docs/mcp-clients.md`, `mcp/src/main.rs`, `mcp/src/tools.rs`, `mcp/tests/thumbnail_content.rs`, … |
| 13 | issue-25-gps-stripped | f588dcf | main | 0/1 | 0/1 | none | none (no PR) | iOS Swift; only the macOS CI leg compiles it. | 4: `mobile/ios/Sources/AlbumenBackup/ImageGPS.swift`, `mobile/ios/Sources/AlbumenBackup/PhotoLibraryBackup.swift`, `mobile/ios/Sources/AlbumenUpload/LocationCheck.swift`, `mobile/ios/Tests/AlbumenUploadTests/UploadCheckTests.swift` |
| 14 | export-honest-rerun-count | eab72a4 | main | 0/3 | 0/3 | none | none (no PR) | Changes export output. Conflicts with 17 and 20 in cli/src/main.rs. | 5: `cli/src/main.rs`, `cli/tests/export_rerun.rs`, `cli/tests/receiver_tombstones.rs`, `ingest/src/export.rs`, … |
| 15 | verify-manifest-proof | f3e580c | export-honest-rerun-count | 0/2 | 0/5 | 14 | none (no PR) | Adds cli/build.rs and a dependency. Conflicts with 17 and 20 (cli/src/main.rs) and 21 (Cargo.lock). | 8: `Cargo.lock`, `cli/Cargo.toml`, `cli/build.rs`, `cli/src/main.rs`, … |
| 16 | exit-codes | a93699b | verify-manifest-proof | 0/2 | 0/7 | 15 | none (no PR) | Touches every error path. Conflicts with 2, 3, 4 (README.md), 17, 18, 20 (cli/src/main.rs) and 21 (Cargo.lock). Needs a rebase after 2–4 merge. | 6: `README.md`, `cli/src/main.rs`, `cli/src/receive.rs`, `cli/tests/exit_codes.rs`, … |
| 17 | issue-40-dry-run-conflicts | d60c7df | main | 0/1 | 0/1 | none | none (no PR) | Conflicts with 14, 15, 16 (cli/src/main.rs), 18 (cli/tests/export.rs) and 20 (ingest/src/lib.rs). | 4: `cli/src/main.rs`, `cli/tests/export.rs`, `ingest/src/checksum.rs`, `ingest/src/lib.rs` |
| 18 | issue-23-rclone-export | aa8ec46 | main | 0/1 | 0/1 | none | none (no PR) | Large rclone change (446 lines). Conflicts with 16, 17, 20. | 3: `cli/src/main.rs`, `cli/tests/export.rs`, `ingest/src/rclone.rs` |
| 19 | issue-22-rclone-smb | 688f8c5 | main | 0/1 | 0/1 | none | none (no PR) | Touches ingest/src/rclone.rs, which 18 also changes; merges cleanly with it today. | 4: `ROADMAP.md`, `docs/architecture.md`, `ingest/src/rclone.rs`, `ingest/tests/roundtrip.rs` |
| 20 | issue-28-card-import | 611c403 | main | 0/4 | 0/4 | none | none (no PR) | New importer (879 lines). Conflicts with 14–18 and 21 in cli/src/main.rs or ingest/src/lib.rs. | 5: `cli/src/main.rs`, `cli/tests/card.rs`, `ingest/src/card.rs`, `ingest/src/lib.rs`, … |
| 21 | takeout-fidelity | eaeecf4 | main | 0/3 | 0/3 | none | none (no PR) | New read-only command. Conflicts with 15 and 16 (Cargo.lock) and 20 (cli/src/main.rs). | 8: `Cargo.lock`, `cli/Cargo.toml`, `cli/src/main.rs`, `cli/tests/takeout_fidelity.rs`, … |
| 22 | icloud-fidelity | 4487c4d | issue-45-live-photo-edge-tests | 0/1 | 0/2 | 5 | none (no PR) | Changes the iCloud report format. Needs exiftool for ContentIdentifier. | 3: `docs/icloud-report.md`, `ingest/src/icloud.rs`, `ingest/tests/synthetic_live_photo.rs` |

## Merge train

Built 2026-10-29 PDT. Each `-train` branch is the previous one plus the original branch's commits, cherry-picked with `-x`. Conflicts were resolved once, here. The original branches are untouched. Jay merges top to bottom; each compare link shows only that step.

Gate at every step: `cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked -p albumen -p albumen-ingest --all-targets --no-fail-fast` with `ALBUMEN_S3_UNAVAILABLE=1`. The only failure at each step is the MinIO-only test `a_delete_or_tombstone_on_copy_a_never_reaches_the_s3_copy` (two_copy_s3); no MinIO is installed. The gate does not run mcp or receive tests, and it does not cover iOS; clippy compiles mcp and receive only.

Every remote head below has an empty `git diff` against the locally gated tree, and each one descends from the one before it.

| # | Branch | Head | Parent | Conflicts resolved | Gate (test secs) | Compare | Files |
|---|---|---|---|---|---|---|---|
| 1 | issue-38-ci-doc-windows-2025-train | 894090a | main | none | green except MinIO (37s) | [main...issue-38-ci-doc-windows-2025-train](https://github.com/jay-the-maker/albumen/compare/main...issue-38-ci-doc-windows-2025-train) | 1: `docs/ci.md` |
| 2 | issue-47-readme-try-it-train | 9669285 | issue-38-ci-doc-windows-2025-train | none | green except MinIO (24s) | [issue-38-ci-doc-windows-2025-train...issue-47-readme-try-it-train](https://github.com/jay-the-maker/albumen/compare/issue-38-ci-doc-windows-2025-train...issue-47-readme-try-it-train) | 1: `README.md` |
| 3 | readme-quickstart-timed-train2 | 8748599 | issue-47-readme-try-it-train | none | green except MinIO (24s) | [issue-47-readme-try-it-train...readme-quickstart-timed-train2](https://github.com/jay-the-maker/albumen/compare/issue-47-readme-try-it-train...readme-quickstart-timed-train2) | 1: `README.md` |
| 4 | compare-doc-train | defadd5 | readme-quickstart-timed-train2 | none | green except MinIO (24s) | [readme-quickstart-timed-train2...compare-doc-train](https://github.com/jay-the-maker/albumen/compare/readme-quickstart-timed-train2...compare-doc-train) | 3: `.github/workflows/link-check.yml`, `README.md`, `docs/compare.md` |
| 5 | issue-45-live-photo-edge-tests-train | 5e40dd8 | compare-doc-train | none | green except MinIO (39s) | [compare-doc-train...issue-45-live-photo-edge-tests-train](https://github.com/jay-the-maker/albumen/compare/compare-doc-train...issue-45-live-photo-edge-tests-train) | 1: `ingest/src/discover.rs` |
| 6 | issue-46-receive-frontend-tests-train | 008bfc7 | issue-45-live-photo-edge-tests-train | none | green except MinIO (25s) | [issue-45-live-photo-edge-tests-train...issue-46-receive-frontend-tests-train](https://github.com/jay-the-maker/albumen/compare/issue-45-live-photo-edge-tests-train...issue-46-receive-frontend-tests-train) | 1: `receive/src/roundtrip.rs` |
| 7 | issue-138-dir-sync-scale-train | d1dad0f | issue-46-receive-frontend-tests-train | none | green except MinIO (26s) | [issue-46-receive-frontend-tests-train...issue-138-dir-sync-scale-train](https://github.com/jay-the-maker/albumen/compare/issue-46-receive-frontend-tests-train...issue-138-dir-sync-scale-train) | 1: `ingest/tests/dir_sync_scale.rs` |
| 8 | scan-export-scale-train | 32010ee | issue-138-dir-sync-scale-train | none | green except MinIO (25s) | [issue-138-dir-sync-scale-train...scan-export-scale-train](https://github.com/jay-the-maker/albumen/compare/issue-138-dir-sync-scale-train...scan-export-scale-train) | 4: `.github/workflows/scan-export-scale.yml`, `bench/baseline.json`, `bench/src/main.rs`, `scripts/scan-export-scale.py` |
| 9 | issue-mcp-stdio-ndjson-train | 274915f | scan-export-scale-train | none | green except MinIO (27s) | [scan-export-scale-train...issue-mcp-stdio-ndjson-train](https://github.com/jay-the-maker/albumen/compare/scan-export-scale-train...issue-mcp-stdio-ndjson-train) | 5: `mcp/src/io.rs`, `mcp/tests/client.rs`, `mcp/tests/source_readonly.rs`, `mcp/tests/stdio_framing.rs`, … |
| 10 | mcp-real-clients-train | de39401 | issue-mcp-stdio-ndjson-train | none | green except MinIO (26s) | [issue-mcp-stdio-ndjson-train...mcp-real-clients-train](https://github.com/jay-the-maker/albumen/compare/issue-mcp-stdio-ndjson-train...mcp-real-clients-train) | 3: `docs/mcp-clients.md`, `scripts/mcp-sdk-smoke.mjs`, `scripts/mcp-smoke.py` |
| 11 | mcp-thumbnail-image-block-train | 0e70060 | mcp-real-clients-train | none | green except MinIO (27s) | [mcp-real-clients-train...mcp-thumbnail-image-block-train](https://github.com/jay-the-maker/albumen/compare/mcp-real-clients-train...mcp-thumbnail-image-block-train) | 6: `docs/mcp-clients.md`, `mcp/src/main.rs`, `mcp/src/tools.rs`, `mcp/tests/thumbnail_content.rs`, … |
| 12 | issue-25-gps-stripped-train | 3ed36c9 | mcp-thumbnail-image-block-train | none | green except MinIO (26s) | [mcp-thumbnail-image-block-train...issue-25-gps-stripped-train](https://github.com/jay-the-maker/albumen/compare/mcp-thumbnail-image-block-train...issue-25-gps-stripped-train) | 4: `mobile/ios/Sources/AlbumenBackup/ImageGPS.swift`, `mobile/ios/Sources/AlbumenBackup/PhotoLibraryBackup.swift`, `mobile/ios/Sources/AlbumenUpload/LocationCheck.swift`, `mobile/ios/Tests/AlbumenUploadTests/UploadCheckTests.swift` |
| 13 | export-honest-rerun-count-train | 37f93eb | issue-25-gps-stripped-train | none | green except MinIO (37s) | [issue-25-gps-stripped-train...export-honest-rerun-count-train](https://github.com/jay-the-maker/albumen/compare/issue-25-gps-stripped-train...export-honest-rerun-count-train) | 5: `cli/src/main.rs`, `cli/tests/export_rerun.rs`, `cli/tests/receiver_tombstones.rs`, `ingest/src/export.rs`, … |
| 14 | verify-manifest-proof-train | f273fa9 | export-honest-rerun-count-train | none | green except MinIO (39s) | [export-honest-rerun-count-train...verify-manifest-proof-train](https://github.com/jay-the-maker/albumen/compare/export-honest-rerun-count-train...verify-manifest-proof-train) | 8: `Cargo.lock`, `cli/Cargo.toml`, `cli/build.rs`, `cli/src/main.rs`, … |
| 15 | exit-codes-train | 862d1c6 | verify-manifest-proof-train | README.md: "## Try it" stays first, then "## Exit codes". | green except MinIO (39s) | [verify-manifest-proof-train...exit-codes-train](https://github.com/jay-the-maker/albumen/compare/verify-manifest-proof-train...exit-codes-train) | 6: `README.md`, `cli/src/main.rs`, `cli/src/receive.rs`, `cli/tests/exit_codes.rs`, … |
| 16 | issue-40-dry-run-conflicts-train2 | fd8639a | exit-codes-train | cli/src/main.rs `print_export_plan` and export_rerun test; see notes. | green except MinIO (77s) | [exit-codes-train...issue-40-dry-run-conflicts-train2](https://github.com/jay-the-maker/albumen/compare/exit-codes-train...issue-40-dry-run-conflicts-train2) | 6: `README.md`, `cli/src/main.rs`, `cli/tests/export.rs`, `cli/tests/export_rerun.rs`, … |
| 17 | issue-23-rclone-export-train | bc53af0 | issue-40-dry-run-conflicts-train2 | `is_remote()` replaces `starts_with("s3://")`; keeps `IngestError::Usage`; cli/tests/export.rs keeps both test sets. | green except MinIO (50s) | [issue-40-dry-run-conflicts-train2...issue-23-rclone-export-train](https://github.com/jay-the-maker/albumen/compare/issue-40-dry-run-conflicts-train2...issue-23-rclone-export-train) | 3: `cli/src/main.rs`, `cli/tests/export.rs`, `ingest/src/rclone.rs` |
| 18 | issue-22-rclone-smb-train | 14359a1 | issue-23-rclone-export-train | none (clean pick) | green except MinIO (50s) | [issue-23-rclone-export-train...issue-22-rclone-smb-train](https://github.com/jay-the-maker/albumen/compare/issue-23-rclone-export-train...issue-22-rclone-smb-train) | 4: `ROADMAP.md`, `docs/architecture.md`, `ingest/src/rclone.rs`, `ingest/tests/roundtrip.rs` |
| 19 | issue-28-card-import-train | dfdc669 | issue-22-rclone-smb-train | ingest/src/lib.rs and main.rs imports; added the `import-card` help example. | green except MinIO (39s) | [issue-22-rclone-smb-train...issue-28-card-import-train](https://github.com/jay-the-maker/albumen/compare/issue-22-rclone-smb-train...issue-28-card-import-train) | 5: `cli/src/main.rs`, `cli/tests/card.rs`, `ingest/src/card.rs`, `ingest/src/lib.rs`, … |
| 20 | takeout-fidelity-train | 43a30a4 | issue-28-card-import-train | main.rs keeps ImportCard and TakeoutFidelity; Cargo.lock keeps `sha2 0.10.9`; added the `takeout-fidelity` help example. | green except MinIO (39s) | [issue-28-card-import-train...takeout-fidelity-train](https://github.com/jay-the-maker/albumen/compare/issue-28-card-import-train...takeout-fidelity-train) | 7: `cli/Cargo.toml`, `cli/src/main.rs`, `cli/tests/takeout_fidelity.rs`, `docs/takeout-fidelity.md`, … |
| 21 | icloud-fidelity-train | 9ff189c | takeout-fidelity-train | none (clean pick) | green except MinIO (49s) | [takeout-fidelity-train...icloud-fidelity-train](https://github.com/jay-the-maker/albumen/compare/takeout-fidelity-train...icloud-fidelity-train) | 3: `docs/icloud-report.md`, `ingest/src/icloud.rs`, `ingest/tests/synthetic_live_photo.rs` |
| 22 | dry-run-all-conflicts-train | this commit | merge-queue-train | none (new behaviour, one commit) | green except MinIO (53s) | [merge-queue-train...dry-run-all-conflicts-train](https://github.com/jay-the-maker/albumen/compare/merge-queue-train...dry-run-all-conflicts-train) | 3: `cli/src/main.rs`, `cli/tests/export_rerun.rs`, `docs/merge-queue.md` |

Notes:

- merge-queue-train (9512d88) only adds this doc on top of step 21. Step 22 sits on it.
- Conflicts (Product Lead decision, step 22): for a local target, both the dry run and `--apply` list every conflict as `  conflict: <path>`, print `N conflicts, nothing written` and exit 3. `--apply` checks first and writes nothing: no files and no manifest. Tested by `every_conflict_is_listed_and_nothing_is_written_in_dry_run_and_apply` (2 conflicts and 1 identical file).
- issue-40 versus export-rerun: the dry run now prints `  present: X`, `  conflict: X` or `  X` for each file, and it skips tombstones. For local targets it then prints `  already present: P`, `  conflicts: C` and `would export N new, P already present and verified`. On the first conflict it exits 3 (`IngestError::AlreadyExists`), which matches exit-codes. S3 targets keep the "would export up to" message. `same_bytes` was removed because nothing used it. `dry_run_names_a_conflict_that_apply_refuses` now asserts a non-zero exit. README Try it shows `already present: 0` and `conflicts: 0`.
- issue-42-sidecar-names is not in the train. It is blocked on Jay's 4 fixture images.
- Two stray remote branches are left over from a push race. They are unused and Jay can delete them: `readme-quickstart-timed-train` (894090a) and `issue-40-dry-run-conflicts-train` (593544a). The `-train2` names replace them.
- Remote heads differ from local SHAs because they were pushed through the GitHub API. The trees are identical.

## Superseded branches

None. Proof, as of the snapshot:

- `git cherry origin/main origin/<branch>` lists no commit already on `main` (no `-` lines) for any of the 22 branches.
- Each branch is 0 behind its base. No queued branch is an ancestor of another queued branch except the stacks shown in "Depends on".

## Rebases

None were needed for this snapshot: every branch is 0 behind its base and `main`. Merging in this order will make later branches conflict, as listed under Risk. When a branch needs a rebase:

1. Push a tag `archive/<branch>-<oldsha7>` at its current head.
2. Push the rebased result as `<branch>-v2`. Never force-push.
3. Record the tag and the new branch here.

| Branch | Archive tag | Rebased branch |
|---|---|---|
| none yet | | |
