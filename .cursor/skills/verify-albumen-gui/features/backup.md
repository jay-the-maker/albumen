# Back up, Backup health, Proof

## Sub-features

- Back up: `albumen backup --from LIB --to DEST --state WORK/backup` for a
  folder given with `--backup` at start; shows the scan/export/verify steps.
- Backup health: one card per backup folder with the CLI's own state word
  (ok, stale, damaged, not backed up, unreadable) and next step.
- Proof: `albumen verify-manifest … --proof DEST/ALBUMEN-PROOFS/verify-<stamp>`;
  shows Verified with the item count, the proof file path, size and SHA-256.

## How to get to it (user POV)

Sidebar > Backup > Back up / Backup health / Proof. Needs `--backup DIR` at
start (`albumen gui --library LIB --work WORK --backup DIR`); without one the
pickers are empty.

## Driving it with drive.mjs

Start with `run-gui.sh` after adding `--backup` by hand (the script does not
pass one), then:

```json
[{"goto":"#backup"},{"click":"#go-backup"},
 {"waitFn":"!document.getElementById('go-backup').disabled","timeout":120000},
 {"goto":"#health"},{"shot":"health"},
 {"goto":"#proof"},{"click":"#go-verify"},
 {"waitFor":"#verified","timeout":120000},{"shot":"proof"}]
```

## Gotchas

- `--demo` builds four backup folders in the four states, which is the
  quickest way to see Backup health.
- run-gui.sh has no `--backup` option yet; this journey is not covered by
  the scripted launch.
