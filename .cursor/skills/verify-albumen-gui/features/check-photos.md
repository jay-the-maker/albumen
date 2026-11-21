# Check photos

Runs `albumen scan LIB --manifest WORK/scan-<stamp>.jsonl` as a child
process. When it finishes, dates switch from file dates to photo dates
(EXIF DateTimeOriginal or the next best tag) and cameras appear.

## Sub-features

- Progress bar and "k of n files".
- Result lines (`scanned …`, `hashed …`).
- The next scan passes `--previous` with the last manifest.
- Command details toggle shows the exact command line.

## How to get to it (user POV)

Sidebar > Backup > Check photos, then the Check photos button.

## Driving it with drive.mjs

```json
[{"goto":"#photos"},
 {"click":"#go-scan"},
 {"waitFn":"!document.getElementById('go-scan').disabled","timeout":120000},
 {"shot":"check-photos"},
 {"click":"[data-s=all]"},
 {"assert":"/photo dates/.test(document.querySelector('#grid .when').textContent)"}]
```

Side effect to verify: exactly one new `scan-*.jsonl` in `--work`; nothing new in the library.

## Gotchas

- Needs the pinned ExifTool on PATH.
- `albumen scan` refuses a library with a symlink or a control-character
  name (it stops at the first one). The grid skips and counts those; the
  scan does not yet. A journey on a `--odd` library reports this failure.
