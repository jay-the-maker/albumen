Tip: 513579a (switch-from-apple-e2e, step 27)
No GitHub CI run yet; CI runs first when PRs open.
After merge, run the scale workflow once with write_baseline.

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
| 19 | issue-28-card-import-train3 | 9167896 | issue-22-rclone-smb-train | as -train, plus a commit routing the staging-dir `remove_dir` through `dest::remove_empty_dir_best_effort` | green except MinIO (59s); check-dest-writes pass | [issue-22-rclone-smb-train...issue-28-card-import-train3](https://github.com/jay-the-maker/albumen/compare/issue-22-rclone-smb-train...issue-28-card-import-train3) | 7: `cli/src/main.rs`, `cli/tests/card.rs`, `ingest/src/card.rs`, `ingest/src/dest.rs`, `ingest/src/lib.rs`, … |
| 20 | takeout-fidelity-train3 | 3b391fc | issue-28-card-import-train3 | same as -train | green except MinIO (56s); check-dest-writes pass | [issue-28-card-import-train3...takeout-fidelity-train3](https://github.com/jay-the-maker/albumen/compare/issue-28-card-import-train3...takeout-fidelity-train3) | 7: `cli/Cargo.toml`, `cli/src/main.rs`, `cli/tests/takeout_fidelity.rs`, `docs/takeout-fidelity.md`, … |
| 21 | icloud-fidelity-train3 | 9c28a65 | takeout-fidelity-train3 | none (clean pick) | green except MinIO (55s); check-dest-writes pass | [takeout-fidelity-train3...icloud-fidelity-train3](https://github.com/jay-the-maker/albumen/compare/takeout-fidelity-train3...icloud-fidelity-train3) | 3: `docs/icloud-report.md`, `ingest/src/icloud.rs`, `ingest/tests/synthetic_live_photo.rs` |
| 22 | merge-queue-train3 | 7cc5098 | icloud-fidelity-train3 | none (docs only: adds `docs/merge-queue.md`) | green except MinIO (43s); check-dest-writes pass | [icloud-fidelity-train3...merge-queue-train3](https://github.com/jay-the-maker/albumen/compare/icloud-fidelity-train3...merge-queue-train3) | 1: `docs/merge-queue.md` |
| 23 | dry-run-all-conflicts-train3 | a0be1f7 | merge-queue-train3 | none (new behaviour, one commit) | green except MinIO (43s); check-dest-writes pass | [merge-queue-train3...dry-run-all-conflicts-train3](https://github.com/jay-the-maker/albumen/compare/merge-queue-train3...dry-run-all-conflicts-train3) | 3: `cli/src/main.rs`, `cli/tests/export_rerun.rs`, `docs/merge-queue.md` |

Notes:

- train3 (2026-10-29 PDT): `scripts/check-dest-writes.py` failed from step 19 because `ingest/src/card.rs:264` called `std::fs::remove_dir` directly. The fix is now at step 19 (issue-28-card-import-train3 = issue-28-card-import-train plus one commit), and steps 20–26 were re-stacked as `-train3` branches (merge-queue-train3 7cc5098 is now its own step 22). The old `-train` branches for steps 19–22 and the old item 3–5 branches are kept, not deleted; merge the `-train3` ones. All 8 new heads were gated and pass check-dest-writes locally. Steps 1–18 are unchanged and already passed it.
- The card.rs `remove_dir` only ever targets `<library>/.albumen-card-staging`, a directory Albumen creates; it fails if the directory is not empty. It is not on main (4dad880). It never touches card or user data.
- Step 22, merge-queue-train3 (7cc5098), only adds this doc on top of step 21. Step 23 sits on it. The train now has 26 steps: 1–23 here and 24–26 stacked below.
- Conflicts (Product Lead decision, step 23): for a local target, both the dry run and `--apply` list every conflict as `  conflict: <path>`, print `N conflicts, nothing written` and exit 3. `--apply` checks first and writes nothing: no files and no manifest. Tested by `every_conflict_is_listed_and_nothing_is_written_in_dry_run_and_apply` (2 conflicts and 1 identical file).
- Re-gate 2026-10-29 PDT: all 23 remote heads (old numbering: steps 1–22 and merge-queue-train; today's steps 1–21, 22 and 23) re-gated from scratch in one working copy with a fresh target dir. Every step is green except the MinIO test; no result changed. Test seconds: 116 (cold build), 24, 25, 24, 42, 26, 29, 27, 26, 25, 27, 26, 39, 38, 40, 40, 48, 48, 48, 51, 52, 43 (merge-queue-train), 43. Passing tests rise from 145 at step 1 to 190 at old step 22 (today's step 23).
- The original issue-28-card-import branch (611c403) still fails check-dest-writes; use the train3 step instead.
- issue-40 (steps 16 and 23): for a local target, the dry run lists every conflict as `  conflict: <path>`, prints `N conflicts, nothing written` and exits 3. `--apply` does the same: it checks every file first, lists all conflicts, prints `N conflicts, nothing written`, exits 3 and writes nothing (no files, no manifest). With no conflicts the dry run prints `would export N new, P already present and verified` and exits 0. Tombstones are skipped. S3 targets keep the "would export up to" message. Test: `every_conflict_is_listed_and_nothing_is_written_in_dry_run_and_apply`.
- issue-42-sidecar-names is not in the train. It is blocked on Jay's 4 fixture images.
- Two stray remote branches are left over from a push race; see "Do NOT merge".
- Remote heads differ from local SHAs because they were pushed through the GitHub API. The trees are identical.

## Stacked on the train (items 3–6)

Each branch is one step on the one before it, all on the train3 tip. All green except the MinIO test.

| # | Branch | Head | Parent | Conflicts resolved | Gate (test secs) | Compare | Files |
|---|---|---|---|---|---|---|---|
| 24 | switch-from-google-e2e-train3 | ff3551e | dry-run-all-conflicts-train3 | none | green except MinIO (44s); check-dest-writes pass | [dry-run-all-conflicts-train3...switch-from-google-e2e-train3](https://github.com/jay-the-maker/albumen/compare/dry-run-all-conflicts-train3...switch-from-google-e2e-train3) | 1: `cli/tests/switch_from_google.rs` |
| 25 | no-network-audit-fidelity-proof-train3 | 2d7e15b | switch-from-google-e2e-train3 | none; card.rs/dest.rs now come from step 19 | green except MinIO (42s); check-dest-writes pass | [switch-from-google-e2e-train3...no-network-audit-fidelity-proof-train3](https://github.com/jay-the-maker/albumen/compare/switch-from-google-e2e-train3...no-network-audit-fidelity-proof-train3) | 4: `.github/workflows/ci.yml`, `docs/test-ladder.md`, `scripts/check-fidelity-proof-strace.py`, `scripts/fidelity-proof-no-network.sh` |
| 26 | changelog-release-notes-train3 | 9e8e089 | no-network-audit-fidelity-proof-train3 | none | green except MinIO (41s); check-dest-writes pass | [no-network-audit-fidelity-proof-train3...changelog-release-notes-train3](https://github.com/jay-the-maker/albumen/compare/no-network-audit-fidelity-proof-train3...changelog-release-notes-train3) | 2: `CHANGELOG.md`, `docs/release-notes-draft.md` |
| 27 | switch-from-apple-e2e | 513579a | changelog-release-notes-train3 | none (new test plus refusal exit-code fix) | green except MinIO (56s, 192 passed; the e2e test runs in about 2s); check-dest-writes pass | [changelog-release-notes-train3...switch-from-apple-e2e](https://github.com/jay-the-maker/albumen/compare/changelog-release-notes-train3...switch-from-apple-e2e) | 3: `cli/tests/switch_from_apple.rs`, `ingest/src/icloud.rs`, `ingest/src/takeout.rs` |

- Step 25's strace audit: first real run in CI. It has not run end to end yet, because the build box has no strace. Its first real run will be in CI. The parser self-test passes, and a fake strace shim was used to check the pipeline. The scripts are mode 100644 on the remote (the API cannot set the exec bit), so CI calls them with `bash`.
- Step 26 adds no tag and no release.
- Step 27 (item 6, switch from Apple): synthetic Photos library (generated Live Photo pair, a JPEG with an XMP sidecar, an edited render) through `import-icloud` dry run and `--apply`, content-id pairing in the report (`pair_basis` content_id 2), export, export re-run (0 new), `verify-manifest --proof` (exit 0), proof again (exit 3), tamper (exit 1, drift 1). It also asserts exit 2 for a missing `--from`, exit 3 for a library inside the source and for an existing report, bit-exact sources, and under 120 s.
- Step 27 changes import exit codes 4→3 for overlap refusals: `import-icloud` and `import-takeout` now exit 3 (not 4) when the destination or report overlaps the source or library.
- Step 27 also fixes a bug from #51 that its exit-code assertions found: `import-icloud` and `import-takeout` reported an overlapping destination or report as a storage failure (exit 4, with the hint "check the storage credentials and network"). They now return `Refused` (exit 3), as the README documents. An existing iCloud report is now `AlreadyExists` (exit 3), as takeout already did. The two `in_place_destination_is_refused` unit tests now assert `Refused` and exit 3.
- mcp and receive suites (`cargo test --locked -p albumen-mcp -p albumen-receive --all-targets`), 2026-10-29 PDT: steps 24 ff3551e (21s), 25 2d7e15b (6s), 26 9e8e089, the train3 tip (7s), and 27 513579a (17s). 72 passed on each, no failures, so no fix branch was needed.

## Do NOT merge

All of these are kept (nothing deleted). Merge only the 26 train steps above in order, then step 27.

| Branch | Head | Reason |
|---|---|---|
| issue-28-card-import-train | dfdc669 | Replaced by step 19 issue-28-card-import-train3; fails check-dest-writes (`ingest/src/card.rs:264` `remove_dir`). |
| takeout-fidelity-train | 43a30a4 | Replaced by step 20 takeout-fidelity-train3; sits on the failing step 19. |
| icloud-fidelity-train | 9ff189c | Replaced by step 21 icloud-fidelity-train3; sits on the failing step 19. |
| merge-queue-train | 9512d88 | Replaced by step 22 merge-queue-train3; sits on the old icloud-fidelity-train. |
| dry-run-all-conflicts-train | b0f8be1 | Replaced by step 23 dry-run-all-conflicts-train3; fails check-dest-writes through step 19. |
| switch-from-google-e2e | 120c6a5 | Replaced by step 24 switch-from-google-e2e-train3; stacked on the old train. |
| no-network-audit-fidelity-proof | 62318ee | Replaced by step 25 no-network-audit-fidelity-proof-train3; stacked on the old train. |
| changelog-release-notes | 7cd06ed | Replaced by step 26 changelog-release-notes-train3; stacked on the old train. |
| readme-quickstart-timed-train | 894090a | Stray from a push race; same head as step 1, replaced by readme-quickstart-timed-train2 (step 3). |
| issue-40-dry-run-conflicts-train | 593544a | Stray from a push race; replaced by issue-40-dry-run-conflicts-train2 (step 16). |
| issue-28-card-import | 611c403 | Original branch; fails check-dest-writes (`std::fs::remove_dir` in card.rs). Use step 19. |

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
