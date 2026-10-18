# Design philosophy

Albumen exists so a photograph stays the file you took. This document is the commitment. Code, docs, issues, and agent instructions that disagree with it are wrong.

## First principle

Metadata and original quality are never lost.

An original is stored bit-exact. Albumen does not recompress, transcode, or strip it. EXIF, XMP, IPTC, and GPS stay inside the file. Sidecars stay beside it. Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay together. Edits are non-destructive and live alongside the original. Timestamps and filenames are preserved. When different bytes arrive under a camera filename that is already stored, the new file keeps a free name and the camera filename is written, byte-exact, into a sidecar.

Every copy is checksum-verified. `copy_verify` streams the bytes, fsyncs, reads the destination back, and compares SHA-256. A mismatch deletes only the destination this call just wrote, names both hashes, and fails. The source stays.

That check is CI-gated. Every merge copies the fixture corpus through the local filesystem, MinIO, and rclone, then compares SHA-256, filename, mtime, and an ExifTool dump. `FileModifyDate`, `FileName`, and embedded EXIF, XMP, IPTC, GPS, and QuickTime tags must match. Weakening that comparison so a change can land is a rejection. If a patch cannot preserve something yet, it fails loudly rather than continuing.

## Never delete

The default path never removes or alters a source. There is no delete tool, no CLI move, and no receiver command that erases the library. Camera-roll backup leaves the item on the device.

`BlobStore::delete` rolls back a destination this process just wrote when verification fails. It is not a way to remove a photo the user already has.

`copy_verify_quarantine` is an explicit per-run opt-in. After a verified copy, it renames sources into a trash directory the caller passed for that call. It never unlinks a source. If the trash directory already has that name, the source stays. The CLI and the default MCP server do not call it.

A sidecar is replaced by writing a temp file, fsyncing it, and renaming it over the old sidecar. The previous sidecar is not unlinked first. The first `source_mtime` recorded at commit is write-once. A later conflicting time is appended. It does not replace the first.

## No lock-in

Leaving is one command. `albumen export` copies the whole library, including sidecars and pairs, through `copy_verify`, to a directory or an `s3://` prefix. The source is not modified. A destination file with the same SHA-256 is left in place, so the command can be run again. A different file at that relative path is an error and is not overwritten.

`albumen verify-manifest` hashes the destination and fails if a file is missing, extra, or different. The manifest lists SHA-256, byte length, and the relative path. After that command succeeds, the export is an ordinary tree of the original files. No Albumen process is required to read them.

## Bring your own storage

Albumen is not a hosting service. You bring the storage, and the bytes stay where you put them.

- Local disk, NAS, SMB, SFTP, WebDAV, S3, and Backblaze B2, reached directly or through rclone.
- Google Drive and Dropbox through the synced folder you already have. That folder is a filesystem. Albumen does not call a vendor API that rewrites the file on the way through.
- iCloud is import-only. Apple does not offer a stable API that can write originals back without rewriting them.

SFTP, WebDAV, and B2 go through `RcloneStore`. rclone is the transport and is not vendored. The exact mtime is kept in a sibling verify object because those backends do not store nanoseconds. S3 keeps mtime in `albumen-mtime-ns`. `Last-Modified` is the upload time and is not the photo's timestamp.

Each connector refuses to overwrite, preserves the filename and mtime, and reports done only after the SHA-256 check.

## Respect for the user

Behavior is transparent. A backup that would strip GPS does not start, and the message says why. A checksum mismatch names both hashes. A derivative is not uploaded in place of an original, and that refusal is logged.

Albumen does not use dark patterns. It does not nag, upsell, or scan the library in the background for a purpose the user did not start. Search reads the files and sidecars already in the library when you run it. It does not phone home, and it does not build a second catalog that becomes the only copy of a caption or a GPS point.

Funding is [GitHub Sponsors](https://github.com/sponsors/jay-the-maker), and no other platforms. There is no paid tier that unlocks fidelity, export, or the original file.

## Agent-native

The interface for agents is MCP, first. `albumen-mcp` speaks MCP over stdio. `search` and `browse` are on immediately. Search filters on capture date, camera, lens, a GPS box, and XMP/IPTC keywords and captions by reading the files. `export` and `upload` exist only when that process is started with `--enable-export` or `--enable-upload`. Both write through `copy_verify` and do not overwrite. Calls named delete, remove, unlink, move, or quarantine are refused.

Agent rules, skills, and instructions are capped by this document. An instruction to delete, recompress, strip metadata, or skip the checksum does not add a tool that can do that. An agent is a contributor: the same first principle, the same Developer Certificate of Origin sign-off, and the same CI gate apply. [AGENTS.md](AGENTS.md) is the working guide. Agent instructions do not outrank this file.

## North star

Be good enough that iCloud Photos and Google Photos have to fix the pain their users already live with: libraries that rewrite files, metadata that disappears, exports that are incomplete, and products that keep the only copy.

The metrics are reliability, fidelity, respect, and control. Cost is outside those metrics. A cheaper copy that is missing GPS, a caption, or a byte is a failure.
