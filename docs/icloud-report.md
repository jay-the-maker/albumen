# What the iCloud import report records

`albumen import-icloud` writes one JSON line per file and a summary line at the end.
Albumen does not quietly guess what a file is. Each line says why it was given its kind.

## Classification

Each file line has `kind` and `classification_basis`.

| basis | rule | kinds |
|---|---|---|
| `path rule` | A folder decided it: `originals/` or `Masters/` means original. `renders/`, `derivatives/` or `Edited/` means edited. | original, edited |
| `name rule` | The file name decided it. `.aae`, `.xmp`, `.albumen.json` and `.csv` are sidecars. A stem ending in `-edited` is an edit. A photo or video extension is an original. | sidecar, edited, original |
| `unknown` | No rule matched. The kind is `unknown`, not original. | unknown |

Files of kind `unknown` are still copied and hash-checked, so nothing is lost. They are only left unlabelled.

## Live Photo pairs

A still and a video that share a stem in the same folder get `live_photo_stem` and `pair_basis`.

- `content-id`: ExifTool read a ContentIdentifier from both halves and the two match. The line also carries `content_identifier`.
- `stem`: the pair rests on the file name alone. The line also has a `pair_flag`:
  - `stem_only`: at least one half has no readable ContentIdentifier. ExifTool might be missing, the file might sit inside an archive, or the tag might be absent.
  - `content_id_mismatch`: both halves have identifiers, but they differ. The line carries `content_identifier` and `partner_content_identifier`.

ExifTool only reads the source files. They are never written.

## Summary

The summary line counts the file lines by basis, under `classification_basis` (`path_rule`, `name_rule`, `unknown`). It counts the paired files under `pair_basis` (`content_id`, `stem`, `stem_only`, `content_id_mismatch`). Each half of a pair is counted once.
