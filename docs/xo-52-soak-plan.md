# XO-52: real-device soak plan for "sync is not backup" (#166)

**Status: plan only. Nothing has run on any of Jay's devices.** Jay runs it on his own devices when he OKs it. No agent touches Jay's Mac, phones or RAID.

Every command below is in [`scripts/xo52-soak.sh`](../scripts/xo52-soak.sh). The script was dry-run on synthetic files in a temp dir. CI runs it the same way on every change, in the `soak plan self-test` job, via `bash scripts/xo52-soak.sh selftest`. No real photos or corpus files are used.

## What it proves

Two real copies of a library, with sync and removals exercised over a soak period:
- neither copy loses or changes any existing file;
- every removal stays a signed record (`albumen family hide` or `albumen family tombstone`);
- no original is unlinked.

This is the "Done when" condition in #166. The copy-local receiver tombstone is not runnable yet; see [Receiver hide](#receiver-hide).

## Variables Jay sets

| Variable | Meaning | Example (Jay's choice) |
|---|---|---|
| `SOAK_SOURCE` | **Required.** The real photo folder to copy from. It is only read. | `"$HOME/Pictures/soak-2024-export"` |
| `SOAK_HOME` | **Required.** A new, empty folder for copy A, the logs and the soak key. | `"$HOME/albumen-soak"` |
| `SOAK_B` | **Required.** A new, empty folder on the RAID for copy B. It is never an existing RAID folder. | `"/Volumes/<RAID name>/albumen-soak-B"` |
| `SOAK_NEW` | Optional, per day. A folder of that day's new photos. It is only read. | `"$HOME/Pictures/soak-new-day1"` |
| `SOAK_HIDE` | Optional, per day. SHA-256s to hide, space-separated. | from `logs/FA-day0.jsonl` |
| `SOAK_TOMBSTONE` | Optional, per day. SHA-256s to tombstone, space-separated. | from `logs/FA-day0.jsonl` |
| `SOAK_APPLY` | Unset or `0`: dry runs only. `1`: each step runs as a dry run, then with `--apply`. | `1` |
| `ALBUMEN` | Path to the `albumen` binary, if it is not on `PATH`. | `albumen` |

Run the script from the repo checkout: `bash scripts/xo52-soak.sh day0`, then `bash scripts/xo52-soak.sh day N`.

## Ground rules

- **Read-only sources.** `SOAK_SOURCE` and `SOAK_NEW` are only read. `albumen scan` and `albumen import-icloud` open them read-only, and the daily source scan has to match day 0.
- **The ledger folder.** `albumen scan <dir>` also creates `<dir>.ledger/` *beside* the folder it scans; the folder itself is not written. For `SOAK_SOURCE` this lands in its parent directory. Pick a `SOAK_SOURCE` whose parent directory may hold that folder, for example an exported copy of the photos, not the Photos library itself. Remove the folder in the rollback.
- **Fresh copies.** Copy A (`$SOAK_HOME/A`), the family libraries and copy B (`$SOAK_B`) are new folders that Albumen creates.
- **Plan before apply.** Every write runs first as a dry run. Run each stage with `SOAK_APPLY` unset first and read the plan, then rerun it with `SOAK_APPLY=1`.
- **Checksums.** Verification is by SHA-256 throughout: `copy_verify` on every write, `albumen verify-manifest` on copy B, and the daily scan diff over path, size, mtime and SHA-256.

## Day 0: setup

```bash
export SOAK_SOURCE=... SOAK_HOME=... SOAK_B=...
bash scripts/xo52-soak.sh day0               # dry run: scan + import plan, writes only logs and the ledger
SOAK_APPLY=1 bash scripts/xo52-soak.sh day0  # writes A, B and the family libraries
```

The commands the script runs:

1. `albumen scan "$SOAK_SOURCE" --manifest "$SOAK_HOME/logs/source-day0.jsonl"`
2. `albumen import-icloud --from "$SOAK_SOURCE" --to "$SOAK_HOME/A" --report "$SOAK_HOME/logs/import-A.jsonl"` as a dry run, then with `--apply`. It exits non-zero unless the count in equals the count out and every SHA-256 matches.
3. `albumen export --from "$SOAK_HOME/A" --to "$SOAK_B/library"` as a dry run, then with `--apply`.
4. `albumen verify-manifest --manifest "$SOAK_B/library/ALBUMEN-MANIFEST.txt" --against "$SOAK_B/library"`
5. `albumen family keygen --key "$SOAK_HOME/keys/soak.key" --apply`, then `albumen family init --to "$SOAK_HOME/family" --key ... --name soak --apply`. This creates family library FA.
6. For each original in A: `albumen family add --library <FA> --key ... --from <file> --apply`
7. `albumen family sync --from <FA> --to "$SOAK_B/family/<library id>"` as a dry run, then with `--apply`. This creates family library FB on the RAID.
8. Scan the source, A, B, FA and FB into `logs/<copy>-day0.jsonl`. These are the reference manifests.

## Each soak day (N = 1 … 7, or as long as Jay chooses)

```bash
SOAK_NEW=... SOAK_HIDE="<sha256> ..." SOAK_TOMBSTONE="<sha256>" bash scripts/xo52-soak.sh day N              # dry run
SOAK_NEW=... SOAK_HIDE="<sha256> ..." SOAK_TOMBSTONE="<sha256>" SOAK_APPLY=1 bash scripts/xo52-soak.sh day N
```

The commands the script runs:

1. **New photos.** If `SOAK_NEW` is set: `albumen import-icloud --from "$SOAK_NEW" --to A ...`, then `albumen export --from A --to "$SOAK_B/library"`. Each runs as a dry run, then with `--apply`.
2. **Removals.**
   - `albumen family hide --library <FA> --key ... --sha256 <sha>` for each entry in `SOAK_HIDE`.
   - `albumen family tombstone --library <FA> --key ... --sha256 <sha>` for each entry in `SOAK_TOMBSTONE`.
   - Each runs as a dry run, then with `--apply`, and is logged in `logs/removals.txt`.
3. **Carry the records to B.** `albumen family sync --from <FA> --to <FB>` as a dry run, then with `--apply`. The output must contain `removed: 0`, or the day fails.
4. **Verify.** `albumen verify-manifest` on B.
5. **Diff.** Scan the source, A, B, FA and FB into `logs/<copy>-dayN.jsonl`. The script checks that every day-0 file is still there with the same path, size, mtime and SHA-256. New files are allowed. B's `ALBUMEN-MANIFEST.txt` is rewritten by each export, so it is checked by `verify-manifest` instead. The script prints `day N: PASS` or exits non-zero with `CHANGED OR MISSING: <path>`.

## Receiver hide

**There is no exact command for a receiver-library hide today.** `Library::hide` in `receive/src/store.rs` writes `{stored name}.albumen-tombstone` beside the original. However, no CLI subcommand, receiver route or MCP tool calls it; only the store tests do. This is tracked in #196.

Until #196 lands, the soak covers removals with the signed `family hide` and `family tombstone` records only. #166 is not fully done until the receiver hide is added to this plan. Its success check will be: the tombstone sidecar exists on copy A only, and the original is still on both copies.

## Success criteria

All of these must hold on every soak day:

- The script prints `day N: PASS`.
- Every `verify-manifest` passes, with 0 missing, 0 extra and 0 different.
- No file from day 0 disappears from, or changes in, the source, A, B, FA or FB.
- Every entry in `removals.txt` exists as a signed record, and its original is still in FA and FB.
- Every `family sync --apply` prints `removed: 0` and `replaced: 0`.

Any failure stops the soak. Keep `SOAK_HOME`, `SOAK_B` and the logs exactly as they are, and open an issue linked to #166 with the manifests and the failing output.

## Rollback

- **Sources:** nothing to roll back. They are only read, and the daily scan proves it. Delete the `<SOAK_SOURCE>.ledger/` folder (and `<SOAK_NEW>.ledger/`, if any) that `albumen scan` created beside them.
- **Copies:** A, FA, FB and B are new soak folders. To roll back, delete `"$SOAK_HOME"` and `"$SOAK_B"` by hand, after the logs are copied somewhere safe. The scan ledgers of the copies are inside those folders. No Albumen command deletes anything.
- **Key:** `$SOAK_HOME/keys/soak.key` is only for the soak, and goes with `SOAK_HOME`.
- **Phones:** this plan does not pair a phone or change a camera roll.

## Report back on #166

Post the dates, file counts and total size, the counts of removals by kind, each day's `PASS` line, and the day-0-to-final diff summary for every copy.
