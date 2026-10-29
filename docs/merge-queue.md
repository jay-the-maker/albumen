# Merge queue

Snapshot taken 2026-10-29 at about 1:50 AM PDT against `main` at `4dad880`. Jay merges, in this order. Nothing here is merged, force-pushed, or deleted.

How each column was read:

- **Head** and **ahead/behind**: `git rev-list --left-right --count origin/<base>...origin/<branch>` after `git fetch`. "behind/ahead" means commits on the base that the branch lacks, then commits the branch adds.
- **CI**: GitHub check runs for the branch head (`GET /repos/jay-the-maker/albumen/commits/<branch>/check-runs`). Every branch returned `total_count: 0`. `ci.yml` runs only on pushes to `main` and on pull requests, and no PRs are open, so CI has not run on any of these heads. Each PR will get its checks when Jay opens it.
- **Conflicts**: `git merge-tree --write-tree origin/<a> origin/<b>` for every pair that is not stacked.

Order: docs first, then tests and CI, then MCP and mobile, then export and CLI behaviour, then importers.

| # | Branch | Head | Base | Behind/ahead vs base | Behind/ahead vs main | Depends on | CI | Risk |
|---|---|---|---|---|---|---|---|---|
| 0 | merge-queue | this commit | main | 0/1 | 0/1 | none | none (no PR) | Docs only; the snapshot goes stale as branches merge. |
| 1 | issue-38-ci-doc-windows-2025 | d9cdbf0 | main | 0/1 | 0/1 | none | none (no PR) | One line in docs/ci.md. Low. |
| 2 | issue-47-readme-try-it | 3499670 | main | 0/1 | 0/1 | none | none (no PR) | README only. Conflicts with exit-codes in README.md. |
| 3 | readme-quickstart-timed | 250e2c4 | issue-47-readme-try-it | 0/2 | 0/3 | 2; describe after 14 lands | none (no PR) | README only. Conflicts with exit-codes in README.md. |
| 4 | compare-doc | cb6536c | readme-quickstart-timed | 0/2 | 0/5 | 3 | none (no PR) | Competitor claims age; every one is sourced and dated 2026-10-29. Conflicts with exit-codes in README.md. |
| 5 | issue-45-live-photo-edge-tests | 97b53f0 | main | 0/1 | 0/1 | none | none (no PR) | Test-only, in ingest/src/discover.rs. Low. |
| 6 | issue-46-receive-frontend-tests | 1134a01 | main | 0/1 | 0/1 | none | none (no PR) | Test-only, in receive/src/roundtrip.rs. Low. |
| 7 | issue-138-dir-sync-scale | 7c531b8 | main | 0/1 | 0/1 | none | none (no PR) | New test; partial fix for #138, Windows CPU target not measured. |
| 8 | scan-export-scale | 5005ece | issue-138-dir-sync-scale | 0/2 | 0/3 | 7 | none (no PR) | New weekly CI job. Baseline is a placeholder, so the 20 percent check is not enforced until Jay records one. |
| 9 | issue-42-sidecar-names | fb30349 | main | 0/3 | 0/3 | Jay commits 4 fixture images first | none (no PR) | Blocked on fixture images Jay must add; tests fail without them. |
| 10 | issue-mcp-stdio-ndjson | 5ccb092 | main | 0/2 | 0/2 | none | none (no PR) | Changes MCP stdio framing. P1: standard MCP clients cannot connect without it. |
| 11 | mcp-real-clients | 5f64151 | issue-mcp-stdio-ndjson | 0/1 | 0/3 | 10 | none (no PR) | Docs and smoke scripts. Low. |
| 12 | mcp-thumbnail-image-block | 6159a07 | mcp-real-clients | 0/2 | 0/5 | 11 | none (no PR) | Changes the thumbnail tool's response shape. |
| 13 | issue-25-gps-stripped | f588dcf | main | 0/1 | 0/1 | none | none (no PR) | iOS Swift; only the macOS CI leg compiles it. |
| 14 | export-honest-rerun-count | eab72a4 | main | 0/3 | 0/3 | none | none (no PR) | Changes export output. Conflicts with 17 and 20 in cli/src/main.rs. |
| 15 | verify-manifest-proof | f3e580c | export-honest-rerun-count | 0/2 | 0/5 | 14 | none (no PR) | Adds cli/build.rs and a dependency. Conflicts with 17 and 20 (cli/src/main.rs) and 21 (Cargo.lock). |
| 16 | exit-codes | a93699b | verify-manifest-proof | 0/2 | 0/7 | 15 | none (no PR) | Touches every error path. Conflicts with 2, 3, 4 (README.md), 17, 18, 20 (cli/src/main.rs) and 21 (Cargo.lock). Needs a rebase after 2–4 merge. |
| 17 | issue-40-dry-run-conflicts | d60c7df | main | 0/1 | 0/1 | none | none (no PR) | Conflicts with 14, 15, 16 (cli/src/main.rs), 18 (cli/tests/export.rs) and 20 (ingest/src/lib.rs). |
| 18 | issue-23-rclone-export | aa8ec46 | main | 0/1 | 0/1 | none | none (no PR) | Large rclone change (446 lines). Conflicts with 16, 17, 20. |
| 19 | issue-22-rclone-smb | 688f8c5 | main | 0/1 | 0/1 | none | none (no PR) | Touches ingest/src/rclone.rs, which 18 also changes; merges cleanly with it today. |
| 20 | issue-28-card-import | 611c403 | main | 0/4 | 0/4 | none | none (no PR) | New importer (879 lines). Conflicts with 14–18 and 21 in cli/src/main.rs or ingest/src/lib.rs. |
| 21 | takeout-fidelity | eaeecf4 | main | 0/3 | 0/3 | none | none (no PR) | New read-only command. Conflicts with 15 and 16 (Cargo.lock) and 20 (cli/src/main.rs). |
| 22 | icloud-fidelity | 4487c4d | issue-45-live-photo-edge-tests | 0/1 | 0/2 | 5 | none (no PR) | Changes the iCloud report format. Needs exiftool for ContentIdentifier. |

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
