# Google Takeout import

**First principle: metadata and original quality are never lost.**

`albumen import-takeout` copies a Google Takeout download of Google Photos into a directory you choose. The archives stay where they are. Originals stay bit-exact. Takeout JSON stays byte-for-byte beside the original it describes. `photoTakenTime` is not written into the original.

## What you run

```bash
albumen import-takeout \
  --from ~/Downloads/takeout \
  --to ~/library \
  --report ~/takeout-report.jsonl
```

`--from` is either one archive or a directory of parts. A directory must contain only `.zip`, `.tgz`, and `.tar.gz` files. A nested folder, a symlink, or any other file is an error, so a part is never skipped. Parts are read in file-name order (`-001`, then `-002`). One archive path imports that file and does not look at its neighbors.

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

The archive-relative path is the destination path, so a JSON file Google placed next to a photo stays next to that photo even when the two members arrived in different parts.

These names are paired sidecars. The partner is the media file name, not a rewritten original:

| Takeout name | Partner |
| --- | --- |
| `IMG_0001.jpg.json` | `IMG_0001.jpg` |
| `CLIP.mp4.supplemental-metadata.json` | `CLIP.mp4` |
| `CLIP.mp4.supplemental-metadata(1).json` | `CLIP.mp4` |

The JSON bytes are copied as they appear in the archive. They are not parsed and written back out. A JSON file that is not a paired name, such as `metadata.json`, is still copied byte-for-byte. Other Takeout files, such as an HTML index, are copied the same way. An extension allow-list would drop a file, so there is not one.

`photoTakenTime` is sidecar metadata. While the report is written, the importer scans the stored JSON for the `photoTakenTime` object's `timestamp` string. `creationTime` is not that field. The timestamp is a column on the sidecar's report line. It is not written into EXIF, XMP, IPTC, the file's mtime, or any other byte of the original. A missing `photoTakenTime` stays missing. Albumen does not invent one and does not open the original for write.

## Reconciliation

The report is one JSON object per line.

An archive seal line records the part name, byte length, and SHA-256 before import. Each file line records the archive name, the member path, the destination relative path, the SHA-256, the byte length, `original` or `sidecar`, and `stored`, `already-verified`, or `mismatch`. Sidecar lines also record `partner` and `photo_taken_time`.

The last line is the summary:

```json
{"summary":true,"count_in":5,"count_out":5,"mismatched":0,"archives_unchanged":true}
```

`count_in` is the number of file members. `count_out` is the number of those members whose destination SHA-256 matched the bytes that were read. A repeated member with the same path and the same bytes counts in both, and the file on disk remains one file. `mismatched` counts members that were not verified, plus an archive whose bytes changed.

The process exits 0 only when `count_in` equals `count_out`, `mismatched` is 0, the archives are unchanged, and at least one file member was present. Any other result exits non-zero after the report is written. A checksum failure names both hashes in the file line.

## What this import will not do

- Extract into the download directory or modify an archive.
- Recompress, transcode, or rewrite an original.
- Write `photoTakenTime` into EXIF or use it as the file mtime.
- Pretty-print or otherwise regenerate a Takeout JSON file.
- Drop a member because of its size, its extension, or how many members the download has.
- Follow a symlink, or write through a member path that escapes the library.
