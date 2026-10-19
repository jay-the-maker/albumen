# Google Takeout import

**First principle: metadata and original quality are never lost.**

`albumen import-takeout` copies a Google Takeout download of Google Photos into a directory you choose. The archives, or an extracted folder, stay where they are. Originals stay bit-exact. Takeout JSON stays byte-for-byte beside the original it describes. `photoTakenTime` and GPS are written into a new `.albumen.json` sidecar. They are not written into the original.

Without `--apply` or `--execute` the command prints the plan and writes nothing. A destination inside the source is still an error.

## What you run

```bash
albumen import-takeout \
  --from ~/Downloads/takeout \
  --to ~/library \
  --report ~/takeout-report.jsonl \
  --apply
```

`--derive-xmp` is off unless you pass it. With the flag, each Takeout JSON also produces a new `.xmp` file. That file is not a rewrite of the JSON or the original.

`--from` is one archive, a directory of parts, or an extracted folder. A directory whose entries are only `.zip`, `.tgz`, and `.tar.gz` files is a split download. Parts are read in file-name order (`-001`, then `-002`). One archive path imports that file and does not look at its neighbors. A directory that mixes archives with other files or folders is an error, so a part is never skipped. Any other directory is an extracted folder: every regular file is copied, and a symlink is a mismatch that is not followed.

`--to` is the library. It must not be the archive directory, a folder inside it, or a folder that contains the archives. That refusal is the "do not extract in place" rule.

`--report` is a new JSONL file outside both trees. An existing report is left as it is.

No account, no network, and no prompt. The command reads the files you named and writes the library and the report.

## Read-only streaming

Each part is opened with read access only. Zip members are read one at a time from that file. A `.tgz` or `.tar.gz` part is gzip-decoded and read as a tar stream. Member bytes are copied through a fixed buffer. The buffer is not a maximum file size, and there is no compiled cap on part count, member count, or member size.

The zip central directory is the index the format uses to find members. Payloads are not loaded whole. The importer does not unpack an archive into a temporary tree, does not write next to the parts, and does not call an extract-into-directory API.

Before any copy, each archive is hashed with SHA-256. After the last member, each archive is hashed again. A changed hash is a mismatch. The import exits non-zero, and the report names both digests.

Directory entries are not files and are not counted. Symlinks and other non-regular members are counted as mismatches and are not created in the library. A member path that is absolute, contains `..`, a backslash, a drive prefix, or any other unsafe component is not joined onto the destination, and that member is a mismatch. A `.` component is ignored because it is not a file or directory name. An escape is never a silent skip: the report records it and the process exits non-zero.

## copy_verify

Every regular file member is stored with the same check as `copy_verify`:

1. Stream the member into a new file. `create_new` refuses a path that already exists.
2. Hash the bytes while writing them.
3. Fsync the file.
4. Read the destination back and compare SHA-256.
5. On mismatch, remove that new file, name both hashes, and record a mismatch. The archive is untouched.
6. Set the destination mtime from the archive member's own timestamp, then read it back.

Zip's DOS timestamp has a two-second resolution and no timezone. It is stored as that civil time on the UTC clock. It is not `photoTakenTime`. A tar member keeps its Unix mtime in seconds. A member with no timestamp is not given an invented one; that member is a mismatch.

A destination that already has the same SHA-256 is left in place, including its mtime, and counts as verified. A destination with different bytes is left in place and is a mismatch. Nothing is overwritten.

Running the command again, with a new report path, is the same rule. Identical bytes stay. Different bytes fail.

## Sidecars

The archive-relative path is the destination path, so a JSON file Google placed next to a photo stays next to that photo even when the two members arrived in different parts. Pairing looks at every part before the copy, so a JSON in one part still names the media file from another part in the same album directory.

Takeout writes `{name}.supplemental-metadata.json`, or the older `{name}.json`. The longest JSON name is 51 UTF-16 code units. `.json` is never cut, so the stem before it is at most 46 units. The cut takes characters from the end of `.supplemental-metadata` first, and then from the media name. A `(N)` duplicate marker sits before the media extension on the file (`IMG_1234(1).jpg`) and after the cut on the JSON (`IMG_1234.jpg.supplemental-metadata(1).json`, or `….supple(1).json` when the stem was cut). The marker is not cut, so a numbered JSON can be longer than 51 units. The plain file does not take the numbered JSON, and the numbered file does not take the plain JSON.

An edited file keeps `-edited` in its own name (`IMG_0001-edited.jpg`). Its JSON is named from that edited name, with the same cut. It does not take the original's JSON.

| Takeout name | Partner |
| --- | --- |
| `IMG_0001.jpg.json` | `IMG_0001.jpg` |
| `CLIP.mp4.supplemental-metadata.json` | `CLIP.mp4` |
| `IMG_1234.jpg.supplemental-metadata(1).json` | `IMG_1234(1).jpg` |
| `Screenshot_20231027_123303_Facebook.jpg.supple.json` | `Screenshot_20231027_123303_Facebook.jpg` |
| `PXL_20240101_120000_EXTRA.jpg.supplemental-met.json` | `PXL_20240101_120000_EXTRA.jpg` |
| `IMG_0001-edited.jpg.supplemental-metadata.json` | `IMG_0001-edited.jpg` |

The JSON bytes are copied as they appear in the archive. They are not parsed and written back out. A JSON file that is not a paired name, such as `metadata.json` or `user-generated-memory-titles.json`, is still copied byte-for-byte and listed in the report with `partner` null. Other Takeout files, such as an HTML index, are copied the same way. An extension allow-list would drop a file, so there is not one.

After the copy, `discover` treats that JSON as a sidecar of the original. A later scan or export of the photo carries `IMG_0001.jpg.json` and `IMG_0001.jpg.supplemental-metadata.json`, including a name Takeout shortened to 51 UTF-16 units. The edited file's JSON stays with the edited file.

### Derived XMP

`--derive-xmp` is off by default. When it is on, Albumen reads the stored JSON and writes a new XMP file beside the partner (`IMG_0001.jpg.xmp`). An unpaired JSON gets `{filename}.xmp`. The XMP carries `title`, `description`, `photoTakenTime` as `exif:DateTimeOriginal` in UTC, and `geoData` when latitude and longitude are not both zero. `creationTime` is not copied into the XMP.

The original is not opened for this write. Its bytes and mtime stay the archive member's. The JSON is not rewritten. A derived XMP is an extra report line of kind `derived-xmp`. It is not an archive member, so it is not added to `count_in` or `count_out`. An existing XMP with different bytes is left in place and the import exits non-zero.

### Albumen sidecar

When the stored JSON has `photoTakenTime` or a `geoData` latitude and longitude that are not both zero, Albumen writes `{photo}.albumen.json` beside the photo. The sidecar holds `photoTakenTime`, `photoTakenTimeUtc`, and `latitude`, `longitude`, and `altitude` when those values are present. The photo file is not opened for write. The Takeout JSON is not rewritten. The sidecar is a report line of kind `albumen-sidecar` and is not added to `count_in` or `count_out`.

### Motion Photos

A still and a video in the same directory that share a file-name stem are a Motion Photo pair. Both report lines record `motion_photo_stem`. A still with no video is not given that field. Both files are copied. Neither is derived from the other.

`photoTakenTime` is sidecar metadata. While the report is written, the importer scans the stored JSON for the `photoTakenTime` object's `timestamp` string. `creationTime` is not that field. The timestamp is a column on the sidecar's report line. It is not written into the original's EXIF, XMP, IPTC, mtime, or any other byte. A missing `photoTakenTime` stays missing. Albumen does not invent one and does not open the original for write. The optional derived XMP is a separate new file.

## Reconciliation

The report is one JSON object per line.

An archive seal line records the part name, byte length, and SHA-256 before import. Each file line records the archive name, the member path, the destination relative path, the SHA-256, the byte length, `original` or `sidecar`, and `stored`, `already-verified`, or `mismatch`. Sidecar lines also record `partner` and `photo_taken_time`.

The last line is the summary:

```json
{"summary":true,"count_in":5,"count_out":5,"mismatched":0,"unmatched":1,"archives_unchanged":true}
```

`count_in` is the number of file members. `count_out` is the number of those members whose destination SHA-256 matched the bytes that were read. A repeated member with the same path and the same bytes counts in both, and the file on disk remains one file. `mismatched` counts members that were not verified, plus an archive whose bytes changed, plus a derived XMP or Albumen sidecar that could not be written. Derived lines are not file members and are not added to `count_in` or `count_out`.

`unmatched` counts media files with no Takeout JSON and Takeout JSON names with no media partner. Each of those files has `"unmatched": true` on its report line. They are still copied. A non-zero `unmatched` does not by itself fail the import. An HTML index is copied and is not unmatched. Nothing is dropped because of its size, its extension, or how many members the download has.

The process exits 0 only when `count_in` equals `count_out`, `mismatched` is 0, the archives are unchanged, and at least one file member was present. Any other result exits non-zero after the report is written. A checksum failure names both hashes in the file line.

## What this import will not do

- Extract into the download directory or modify an archive.
- Recompress, transcode, or rewrite an original.
- Write `photoTakenTime` into EXIF or use it as the file mtime.
- Pretty-print or otherwise regenerate a Takeout JSON file.
- Drop a member because of its size, its extension, or how many members the download has.
- Follow a symlink, or write through a member path that escapes the library.
