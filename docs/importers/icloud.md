# iCloud Photos import

**First principle: metadata and original quality are never lost.**

`albumen import-icloud` copies an iCloud Photos download, a Photos export, or a copy of a Photos library into a directory you choose. The source stays where it is. Originals and edited versions stay bit-exact. Live Photo stills and videos stay side by side. AAE and XMP sidecars are copied as stored. A capture time read from a sidecar is not written into an original.

## What you run

```bash
albumen import-icloud \
  --from ~/Pictures/Photos\ Library.photoslibrary \
  --to ~/library \
  --report ~/icloud-report.jsonl
```

`--from` is a directory or one archive. A directory is an export folder or a `.photoslibrary` package. An archive is one `.zip`, `.tgz`, or `.tar.gz` download. A directory source is not treated as a folder of archives: every regular file under it is a member.

`--to` is the library. It must not be the source, a folder inside it, or a folder that contains it. An archive's source root is the directory that holds the archive, so the library cannot sit next to that archive inside the same folder. Put the archive in its own directory.

`--report` is a new JSONL file outside both trees. An existing report is left as it is.

No account, no network, and no prompt. The command reads the files you named and writes the library and the report.

## Read-only

A directory is walked with `symlink_metadata`. A symlink is a mismatch and is not followed. Each regular file is opened for read. After the copies, every source file named in the report is hashed again. A changed hash is a mismatch.

An archive is opened with read access only. Zip members are read one at a time. A `.tgz` or `.tar.gz` file is gzip-decoded and read as a tar stream. Member bytes are copied through a fixed buffer. The buffer is not a maximum file size. There is no compiled cap on directory size, member count, or member size.

The zip central directory is the index the format uses to find members. Payloads are not loaded whole. The importer does not unpack an archive into a temporary tree and does not write next to the archive.

Before any copy, an archive is hashed with SHA-256. After the last member, it is hashed again. A changed hash is a mismatch.

Directory entries are not files and are not counted. Symlinks and other non-regular members are counted as mismatches and are not created in the library. A member path that is absolute, contains `..`, a backslash, a drive prefix, or any other unsafe component is not joined onto the destination, and that member is a mismatch. A `.` component is ignored. An escape is never a silent skip: the report records it and the process exits non-zero.

Package files inside a Photos library, including SQLite and other non-media files, are copied too. Dropping them would make the count in differ from the count out.

## copy_verify

Every regular file is stored with the same check as `copy_verify`:

1. Stream the bytes into a new file. `create_new` refuses a path that already exists.
2. Hash the bytes while writing them.
3. Fsync the file.
4. Read the destination back and compare SHA-256.
5. On mismatch, remove that new file, name both hashes, and record a mismatch. The source is untouched.
6. Set the destination mtime from the source file's mtime, or from the archive member's own timestamp, then read it back.

That mtime is the file's timestamp. It is not a capture time parsed from a sidecar. Zip's DOS timestamp has a two-second resolution and no timezone. It is stored as that civil time on the UTC clock. A tar member keeps its Unix mtime in seconds. A member with no timestamp is not given an invented one; that member is a mismatch.

A destination that already has the same SHA-256 is left in place, including its mtime, and counts as verified. A destination with different bytes is left in place and is a mismatch. Nothing is overwritten.

Running the command again, with a new report path, is the same rule. Identical bytes stay. Different bytes fail.

## Originals, edits, Live Photos, sidecars

The source-relative path is the destination path. An edited render stays beside the original. It does not replace it.

| Path | Report `kind` |
| --- | --- |
| `.aae` or `.xmp`, any case | `sidecar` |
| A path component `renders`, `derivatives`, or `edited`, or a file name stem ending in `-edited` | `edited` |
| A still, video, or RAW name, or a path under `originals` or `Masters` | `original` |
| Anything else that was still copied | `file` |

A Live Photo is the still and the video that share a file-name stem. The report records `live_photo_stem` on `.jpg`, `.jpeg`, `.heic`, `.heif`, `.hif`, `.png`, `.tif`, `.tiff`, `.avif`, `.webp`, `.mov`, `.mp4`, and `.m4v`. Both files are copied. Neither is derived from the other.

A sidecar's `partner_stem` is the file name with `.xmp` or `.aae` removed, and with a trailing media extension removed when one is there. `IMG_0001.HEIC.xmp` and `IMG_0001.AAE` both partner `IMG_0001`.

`DateTimeOriginal` is sidecar metadata. While the report is written, the importer scans the stored sidecar for that key and the timestamp token that follows it. The token is a column on the sidecar's report line. It is not written into EXIF, XMP, IPTC, the file's mtime, or any other byte of an original. A sidecar with no `DateTimeOriginal` stays without one. Albumen does not invent a capture time and does not open an original for write.

## Reconciliation

The report is one JSON object per line.

An archive seal line records the archive name, byte length, and SHA-256 before import. Each file line records the source name, the member path, the destination relative path, the SHA-256, the byte length, the kind, and `stored`, `already-verified`, or `mismatch`. Sidecar lines also record `partner_stem` and `capture_time`.

The last line is the summary:

```json
{"summary":true,"count_in":6,"count_out":6,"mismatched":0,"sources_unchanged":true}
```

`count_in` is the number of regular files, plus non-regular members that were refused. `count_out` is the number whose destination SHA-256 matched the bytes that were read. `mismatched` counts members that were not verified, plus a source whose bytes changed. `sources_unchanged` is the archive seal or the second hash of the directory. A destination that already held different bytes sets `mismatched` and leaves `sources_unchanged` true when the source itself did not change.

The command exits 0 only when `count_in` equals `count_out`, `mismatched` is 0, every source is unchanged, and `count_in` is greater than 0. An empty source is a mismatch. The report is still written.
