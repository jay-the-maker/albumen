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

## Performance and friction

Performance is at a premium. User friction is minimal.

Both are measured, and a change that misses the measurement does not land. [docs/performance-budgets.md](docs/performance-budgets.md) states the budgets: cold start, time to the first thumbnail, scroll frame rate on a 380,000-item library, search latency at p50 and p95, import throughput in MB/s and items/s, and backup CPU and battery. Cold start, thumbnails, scroll, and backup CPU and battery are targets until the apps exist. Import throughput, metadata search, and MCP response time exist now. CI repeats those runs and keeps a baseline. It fails when copy throughput, cold metadata search, or browse response time regresses by more than 10 percent against a same-run reference. A noisy repeat is dropped before that comparison. Warm search p50 and p95 are printed with every run. They are not a CI failure, because that ratio is not stable across the hosted runners. A local run of the same benchmark at 380,000 items is how the full-library numbers are taken. CI scales the library down so the job finishes.

[docs/friction.md](docs/friction.md) counts the steps from install to a first backup. The target is a handful, five steps or fewer. Sane defaults, no account, and no nags are hard rules. The pull request template asks for them, and the philosophy check fails unless those boxes are checked.

Speed is not a reason to drop a byte, a tag, or a checksum. Cost stays outside the metrics.

## Scale

Scales with your hardware, no artificial limits.

There is no hard-coded ceiling on library size, file size, concurrency, or item counts. A page size, a batch size, or a worker count is a soft default the user can override. It is not a maximum.

Concurrency follows the machine. CPU count, IO saturation, and network backpressure decide how much work runs at once. A fixed pool that ignores the hardware is a ceiling.

APIs stream or paginate. They do not load the whole library to answer one page. An index may grow to tens of millions of items. The files and sidecars stay the source of truth: an index is a cache the files can rebuild, not a second catalog that becomes the only copy of a caption or a GPS point. Memory stays constant as the library grows. A response holds one page, not the library.

Under throttling, work slows and continues. It does not fail because a count, a file, or a queue crossed a number compiled into the binary.

A bound that protects the first principle, or that protects the process from unauthenticated input, stays. Protocol-level denial-of-service limits are that kind of bound. Each one is documented and configurable. A limit that exists only to keep a demo small does not stay.

A larger library is not a reason to drop a byte, a tag, or a checksum.

## North star

Be good enough that iCloud Photos and Google Photos have to fix the pain their users already live with: libraries that rewrite files, metadata that disappears, exports that are incomplete, and products that keep the only copy.

The metrics are reliability, fidelity, respect, control, scale, and the measured performance and friction budgets. Cost is outside those metrics. A cheaper copy that is missing GPS, a caption, or a byte is a failure. A faster copy that drops them is a failure too. A larger library that drops them is a failure too.
