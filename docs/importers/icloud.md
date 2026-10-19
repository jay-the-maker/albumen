# iCloud Photos import

**First principle: metadata and original quality are never lost.**

`albumen import-icloud` copies an iCloud Photos download, a Photos export, or a copy of a Photos library into a directory you choose. The source stays where it is. Originals and edited versions stay bit-exact. Live Photo stills and videos stay side by side. AAE and XMP sidecars are copied as stored. Capture time and GPS are written into a new `.albumen.json` sidecar. They are not written into an original.

Without `--apply` or `--execute` the command prints the plan and writes nothing. A destination inside the source is still an error.

## What you run

```bash
albumen import-icloud \
  --from ~/Pictures/Photos\ Library.photoslibrary \
  --to ~/library \
  --report ~/icloud-report.jsonl \
  --apply
```

`--from` is a directory or one archive. One archive is a `.zip`, `.tgz`, or `.tar.gz` file. A directory that contains only those archives and `.csv` files is a multi-part iCloud download. Any other directory is an export folder or a `.photoslibrary` package, and every regular file under it is a member.

`--to` is the library. It must not be the source, a folder inside it, or a folder that contains it. An archive's source root, including each part of a multi-part download, is the directory that holds the archives, so the library cannot sit next to those parts inside the same folder. Put the download in its own directory.

`--report` is a new JSONL file outside both trees. An existing report is left as it is.

No account, no network, and no prompt. The command reads the files you named and writes the library and the report.

## Read-only

A directory is walked with `symlink_metadata`. A symlink is a mismatch and is not followed. Each regular file is opened for read. After the copies, every source file named in the report is hashed again. A changed hash is a mismatch.

An archive is opened with read access only. A multi-part download is a directory of `.zip`, `.tgz`, and `.tar.gz` parts plus CSV files such as `Photo Details.csv` and `Album Details.csv`. Parts are read in file-name order. Zip members are read one at a time. A `.tgz` or `.tar.gz` file is gzip-decoded and read as a tar stream. CSV files are not parsed into a new file. They are copied byte-for-byte and reported as sidecars. Member bytes are copied through a fixed buffer. The buffer is not a maximum file size. There is no compiled cap on directory size, part count, member count, or member size.

The zip central directory is the index the format uses to find members. Payloads are not loaded whole. The importer does not unpack an archive into a temporary tree and does not write next to the archive.

Before any copy, an archive is hashed with SHA-256. After the last member, it is hashed again. A changed hash is a mismatch. A directory that is not a multi-part download is hashed again file by file after the copy. A generated metadata sidecar is not a source file, so that second pass does not look for it in the library.

Directory entries are not files and are not counted. Symlinks and other non-regular members are counted as mismatches and are not created in the library. A member path that is absolute, contains `..`, a backslash, a drive prefix, or any other unsafe component is not joined onto the destination, and that member is a mismatch. A `.` component is ignored. An escape is never a silent skip: the report records it and the process exits non-zero.

Package files inside a Photos library, including the database, are copied too. Dropping them would make the count in differ from the count out. The copy of the database is the bytes from the library. It is not a rewrite.

## Album, favorite, and caption

When the copy contains `database/Photos.sqlite` or `Photos.sqlite` and the file is a SQLite database, Albumen opens it with the URI query `mode=ro&immutable=1` and sets `PRAGMA query_only=ON`. Immutable mode does not create `Photos.sqlite-wal` or `Photos.sqlite-shm`, does not apply a journal, and does not write the database. The library file is left as it was, then copied byte-for-byte with the rest of the tree.

Album names, a favorite mark, and a caption are written next to the original as `{filename}.albumen.json`. The JSON is a new sidecar. It is not written into the original and it is not written back into the Photos database. A file with no favorite, no caption, and no album does not get an empty sidecar. Album names are kept in full and sorted. The sidecar's file mtime is the original's mtime, not a capture time parsed from the database.

A Photos library whose database is not SQLite, such as a stand-in file in a test, is copied as a file and does not grow a metadata sidecar. A database that names a photo that was not copied is a mismatch.

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
| `.aae`, `.xmp`, `.csv`, or `.albumen.json` | `sidecar` |
| A path component `renders`, `derivatives`, or `edited`, or a file name stem ending in `-edited` | `edited` |
| A still, video, or RAW name, or a path under `originals` or `Masters` | `original` |
| Anything else that was still copied | `file` |

A Live Photo is the still and the video that share a file-name stem in the same directory. The report records `live_photo_stem` on both files only when both are present. A still with no video does not get the field. Both files are copied. Neither is derived from the other. A sidecar whose partner file is not in that directory is `"unmatched": true` and is still copied.

A sidecar's `partner_stem` is the file name with `.xmp`, `.aae`, or `.albumen.json` removed, and with a trailing media extension removed when one is there. `IMG_0001.HEIC.xmp`, `IMG_0001.AAE`, and `IMG_0001.HEIC.albumen.json` all partner `IMG_0001`. A Photo Details CSV describes the download, not one file, so its `partner_stem` is empty.

`DateTimeOriginal` is sidecar metadata. While the report is written, the importer scans the stored sidecar for that key and the timestamp token that follows it. The token is a column on the sidecar's report line. It is not written into EXIF, XMP, IPTC, the file's mtime, or any other byte of an original. A sidecar with no `DateTimeOriginal` stays without one. Albumen does not invent a capture time and does not open an original for write.

## Reconciliation

The report is one JSON object per line.

An archive seal line records the archive name, byte length, and SHA-256 before import. Each file line records the source name, the member path, the destination relative path, the SHA-256, the byte length, the kind, and `stored`, `already-verified`, or `mismatch`. Sidecar lines also record `partner_stem` and `capture_time`.

The last line is the summary:

```json
{"summary":true,"count_in":6,"count_out":6,"mismatched":0,"unmatched":0,"sources_unchanged":true}
```

`count_in` is the number of regular files, plus non-regular members that were refused. `count_out` is the number whose destination SHA-256 matched the bytes that were read. `mismatched` counts members that were not verified, plus a source whose bytes changed. `sources_unchanged` is the archive seal or the second hash of the directory. A destination that already held different bytes sets `mismatched` and leaves `sources_unchanged` true when the source itself did not change.

The command exits 0 only when `count_in` equals `count_out`, `mismatched` is 0, every source is unchanged, and `count_in` is greater than 0. An empty source is a mismatch. The report is still written.
