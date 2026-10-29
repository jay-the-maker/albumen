# Takeout fidelity report

`albumen takeout-fidelity` shows, for every original in a Google Takeout
import, what the Takeout JSON sidecar says and what the original's own EXIF
says about capture time and GPS. It does not change anything: the report and
the library are opened read-only, and the only file written is the new
fidelity report, which must not already exist.

```sh
albumen import-takeout --apply --from ~/Takeout --to ~/Library --report import.jsonl
albumen takeout-fidelity --report import.jsonl --library ~/Library --out fidelity.jsonl
```

## Which value wins

EXIF wins when the original has the field, because it travels inside the
original's bytes. The JSON value wins only when EXIF lacks the field. The
winner is reported; nothing is written into any original.

## Thresholds and flags

| Flag | Meaning |
|---|---|
| `time_disagree` | JSON and EXIF capture times differ by more than 60 seconds |
| `gps_disagree` | JSON and EXIF positions are more than 100 metres apart (haversine) |
| `time_missing_json` / `time_missing_exif` | that source has no capture time |
| `gps_missing_json` / `gps_missing_exif` | that source has no position; Google's `0,0` counts as missing |
| `time_unzoned` | EXIF has `DateTimeOriginal` but no `OffsetTimeOriginal`, so it cannot be compared with the JSON's UTC time |
| `no_sidecar` | the import found no JSON for this original |

A missing value is reported as `null` and flagged. It is never filled in or
inferred. The last line is a `fidelity-summary` with a count for every flag.

Tests: `cli/tests/takeout_fidelity.rs` builds five synthetic cases (agree,
disagree, JSON-only, EXIF-only, neither) from the public fixture with its
metadata stripped and rewritten.
