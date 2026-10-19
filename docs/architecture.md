# Architecture

**First principle: metadata and original quality are never lost.**

Originals are stored bit-exact: Albumen does not recompress, transcode, or strip them. EXIF, XMP, IPTC, and GPS stay. Sidecars stay. Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay. Edits are non-destructive and are stored alongside the originals. Timestamps and filenames are preserved. Every copy is checked with SHA-256 and fails loudly on mismatch. The default path does not move or delete a source.

This is the outline of the system. The ingest crate, the export command, and the stdio MCP skeleton exist today. The rest is the shape later crates have to fit.

## Status

| Piece | State |
| --- | --- |
| Charter, license, contribution rules | Here |
| `albumen-ingest` copy/verify for local files, S3, and rclone (SFTP, WebDAV, B2) | Here |
| Fixture corpus and MinIO round-trip CI | Here |
| Catalog / search index | No on-disk catalog. MCP search reads files and sidecars into a process-local cache |
| macOS, iOS, and Android apps | iOS is a PhotoKit library. Android `mobile/android/host` backs up the camera roll to `albumen-receive`. It is not published to a store. The macOS library UI is not started |
| MCP server (`albumen-mcp`, stdio) | Here |
| Camera-to-cloud (FTP/SFTP/WebDAV/S3 and USB) | `albumen-receive` speaks WebDAV, SFTP, and FTP into `copy_verify`. S3 upload and USB are not started |
| Export-everything command (`albumen export`) | Here |
| Google Takeout import (`albumen import-takeout`) | Here. Streams split zip and tgz parts read-only. See [importers/takeout.md](importers/takeout.md) |
| iCloud Photos import (`albumen import-icloud`) | Here. Reads a download, export, or Photos library copy. See [importers/icloud.md](importers/icloud.md) |
| rclone-backed SMB, SFTP, WebDAV, B2 | SFTP, WebDAV, and B2 write through `RcloneStore` and `copy_verify`. SMB is not started |

## Shape

```text
macOS app ──────────────┐
iOS / Android backup ───┤
camera FTP/SFTP/WebDAV ─┼─► albumen-ingest copy_verify ─► BlobStore
USB / tethered import ──┤         │                        local / S3 / rclone
MCP upload (opt-in) ────┘         ▼
                          originals + sidecars + edit files
                                  │
                                  ▼
                          catalog (rebuildable)
                                  │
                                  ▼
                          MCP: search, browse, export
                          upload/ingest only if the user enables it
```

The bytes on disk are the library. The catalog is a cache. If the catalog is deleted, it can be rebuilt by reading the originals. If an original is lost, nothing rebuilds it, so ingest is not allowed to risk it.

## Ingest contract

`copy_verify` in `albumen-ingest`:

1. Take a primary file. Discover companions in the same directory.
2. Copy bytes with `read` and `write`. Do not decode the image.
3. `fsync` the destination.
4. Read the destination back and compute SHA-256. Compare it to the hash of the bytes just written.
5. On mismatch, delete the destination and return `IngestError::ChecksumMismatch` with both hex digests.
6. Set the destination mtime to the source mtime. Read it back. If the filesystem did not keep it, delete the destination and return `MtimeNotPreserved`.
7. Keep the filename exactly. A requested name that differs from the source name is an error. Overwriting an existing destination is an error.
8. If a later file in the asset fails, delete the destination files this call already wrote. The source is unchanged.

`copy_verify` is the only default path. `copy_verify_quarantine` is an explicit per-run opt-in. After the copy verifies, it renames each source into a trash directory the caller supplies for that call. It never unlinks a source. If the rename would overwrite something in the trash, the source stays. The CLI does not call it.

A stored original is not unlinked after that check. The receiver refuses WebDAV `DELETE` and `MOVE`, SFTP remove and rename, and FTP `DELE`, `RMD`, and rename. If the receipt sidecar cannot be written, or the library is asked to hide a file, Albumen writes `{filename}.albumen-tombstone` and leaves the original in place. Importers and the MCP server have no remove tool. `BlobStore::delete` and an export rollback remove only a destination this call just wrote, before that call has finished.

Android motion photos are one JPEG with an embedded video and XMP. They stay intact because the file is never rewritten. There is no separate pair to discover.

## Companions discovered today

- Sidecars: `{stem}.xmp`, `{filename}.xmp`, and the same patterns for Apple `.aae` edit sidecars, either case. The `.aae` is an edit list, not a replacement for the still, and discovery copies the pair together.
- Pairs, same stem, same directory:
  - still (JPEG, HEIC, PNG, TIFF, AVIF, WebP) with RAW
  - still with video (Live Photo)
  - RAW with still
  - video with still

A non-UTF-8 name that looks like a pair is an error, not a skip. Skipping would drop a companion.

Android motion photos and Ultra HDR gain maps are one file each. The motion photo is a JPEG with an MP4 after the EOI and XMP offsets that point at that trailer. The Ultra HDR JPEG carries an ICC profile, gain-map XMP, and a second JPEG in an MPF segment. Discovery does not split them. The byte-exact copy is what keeps the trailer, the profile, and the gain map. The CI corpus round-trips both, plus an `.aae` sidecar.

## Storage

`BlobStore` is the seam: `put_verified`, `get_verified`, `delete`.

- `LocalStore` is a directory. Synced Google Drive and Dropbox folders are this store. Albumen does not talk to those vendors' APIs.
- `S3Store` speaks the S3 API with path-style addressing, so MinIO, AWS S3, and other S3-compatible endpoints work. Checksums on the request are sent only when required, which is what MinIO accepts.
- SMB, SFTP, WebDAV, and B2 are planned through rclone, or through B2's S3 API for the second. They are not implemented.

S3 does not have a Unix mtime. `Last-Modified` is the upload instant and must not be shown as the time the photo was taken or the file was last modified. The original mtime is user metadata `albumen-mtime-ns` (signed Unix nanoseconds). The source SHA-256 is `albumen-sha256`. After `PUT`, the object is read back and hashed. If the metadata did not stick, or the body hash differs, the object is deleted and the call fails.

`GET` refuses to restore an object that is missing either metadata field. It writes the body, hashes it, compares it to `albumen-sha256`, then sets the local mtime from `albumen-mtime-ns`.

## Edits

Edits never replace the original. A rating, a crop, or a color adjustment is a sidecar (XMP or `.aae` today; an Albumen edit file later) stored next to the original under the same stem. Rendering a preview is allowed to encode a new file. That file is a derivative, in a different place, with a different name. The original stays.

## Catalog

MCP search does not keep a catalog. It reads capture date, camera, lens, GPS, keywords, and captions from each file and from XMP or AAE sidecars beside it. A process-local cache is reused only while the file's SHA-256 and length are unchanged. Nothing is written next to the originals.

A future catalog may store ids, paths, filenames, mtimes, SHA-256, and extracted metadata so a browser can scroll without reading every file. It would be rebuildable. A hash in that catalog is a hint; a copy still hashes the bytes. The catalog is not allowed to be the only copy of GPS, captions, or dates.

## Apps

- macOS: SwiftUI for the library UI, AppKit where SwiftUI still falls short (import panels, drag and drop, menus). It links or shells out to the ingest core. That UI is not started.
- iOS and Android: `mobile/ios` is a PhotoKit library. It writes `PHAssetResource` originals, including a Live Photo's paired video, with `isNetworkAccessAllowed` set so an iCloud original is downloaded. It does not call `PHImageManager.requestImage`, and it does not upload a derivative when the full original is missing. The background extension is scoped in [camera-roll-scope.md](camera-roll-scope.md). `mobile/android/host` is the Android backup app. It requests media and location permission and enqueues only when both are granted. CI does not click that dialog. Android opens MediaStore with `setRequireOriginal` and `ACCESS_MEDIA_LOCATION`. The first WorkManager job runs on a network and is aimed at a verified copy within 5 minutes of setup. Later jobs run while the device is idle, charging, and on a network. The receiver password is in Keystore-backed storage on Android and in the Keychain on iOS. WorkManager input carries the endpoint and the user only. Both PUT the bytes to `albumen-receive` and accept the copy only when `X-Albumen-Sha256` matches the client's SHA-256. A mismatch is retried and shown. The camera-roll original stays. The Android path is [mobile-android-backup.md](mobile-android-backup.md). The apps do not transcode to "save space" on the way in. The Android debug APK is not published to a store.

iCloud is import-only. A user can copy photos out of iCloud into the library. Albumen does not write back into iCloud Photos, because that path rewrites containers and does not promise bit-exact originals.

## Camera to cloud

Phone camera rolls are not the only ingest path. Dedicated cameras deliver RAW and video the same way, through `copy_verify`.

- Wireless. `albumen-receive` is the process the user runs on their own machine. It listens for WebDAV, SFTP, and plain FTP. A camera uploads each finished file. The receiver stores it only after the upload is complete, through `copy_verify`. A partial file is not reported done. WebDAV's 201 response includes `X-Albumen-Sha256`, the digest `copy_verify` checked. A different file with the same camera filename, such as `IMG_9999` after the counter wraps or a second camera, is stored under a free name. The camera filename is kept byte-exact, and the uploaded directory such as `DCIM/100CANON` is kept, both in a `.albumen.json` sidecar. An upload of the same bytes is a 409 with that digest and is not stored again. FTP `MFMT` and `MDTM`, SFTP attributes during the upload, and the WebDAV header `X-Albumen-Mtime` set the camera mtime only inside the session that creates the file. The sidecar keeps that first `source_mtime` apart from `received_at`. A later `MFMT`, `setstat`, or identical-byte retry appends a conflict and does not replace the stored mtime. The sidecar itself is replaced by an fsynced rename, never deleted first. JPEG, RAW, and video share this path. FTP is unencrypted and is there for older bodies. S3 upload from a camera and tethered USB import are not in this receiver.
- Tethered. USB import, whether PTP/MTP or a mounted card, copies each selected file with `copy_verify`. Ingest does not erase the card.

Both paths refuse to overwrite an existing original. Success is the verified destination, not the moment the socket closed.

## Agents

The MCP server covers search, browse, export, and upload/ingest.

Read tools are the default: search files and sidecars by capture date, camera, lens, GPS box, keyword, and caption; browse by path; and export a copy to a path the user chose. Export is `copy_verify` out of the library. It is not a move, and it is not a delete.

Upload and ingest are write tools. They stay off until the user explicitly enables them. A write calls `copy_verify` and reports success only after the destination hash and mtime have been checked. It never overwrites an existing original, and it never recompresses one.

There is no delete tool in the default set or in the enabled write set. There is no tool that rewrites an original in place.

## Scale

Hundreds of thousands of items and multiple terabytes means:

- Stream copies and hashes. Do not read a video into memory.
- One file failure does not stop the process silently; it is reported, and that file is not marked stored.
- The catalog is an index, not a blob store.
- Filenames stay as the user named them. Content-addressed paths, if they are added later, are an internal location and never a rename of the original the user sees.
- Derivatives (thumbnails, previews) are disposable and live apart from originals.

## What this system will not do

- Recompress, transcode, or strip metadata on import.
- "Fix" orientation by rewriting the file.
- Drop GPS for privacy by default. A privacy export can write a new file. The original keeps its GPS.
- Rename `IMG_0001.JPG` to a UUID.
- Treat a database row as the photo.
- Offer an agent a delete tool.

## Why the ingest core is Rust

The macOS and iOS apps will be Swift, and the Android app will be Kotlin. The piece that must not lose a byte has to run in CI on Linux, and later it has to be callable from all three clients. A Rust library can do that: explicit errors, streaming SHA-256, filesystem timestamps, and an S3 client, without a garbage-collected runtime in the copy path. Swift packages cannot be the CI gate on a Linux runner. The crate is small on purpose. UI, face detection, and the catalog do not belong in it.

## CI

GitHub Actions runs `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` on `ubuntu-24.04` and `ubuntu-26.04`. A separate `ubuntu-24.04` job unit-tests the Android backup and assembles the debug APK. Another job runs the MediaStore instrumented test on an API 30 emulator on `ubuntu-latest`, with KVM and `reactivecircus/android-emulator-runner`. A `macos-14` job builds the iOS package. The runner pin, the ExifTool 13.59 checksum, the MinIO checksum, and the Android SDK pin are in [ci.md](ci.md). MinIO is AGPL. The workflow downloads it and runs it; the Albumen tree does not contain MinIO source. The S3 credentials in CI are fixed test strings (`albumen` / `albumen-ci-not-a-secret`) for a server that exists only for that job.

The fixture corpus is synthetic and CC0. It includes a Motion Photo (JPEG plus appended MP4), an Apple `.aae` edit sidecar, and an Ultra HDR JPEG with a synthetic ICC profile and an MPF gain map, along with the earlier GPS, HEIC, RAW+JPEG, XMP, Live Photo, and video files. Tests compare each file with itself after a round trip. They do not depend on a checked-in ExifTool golden file, so an ExifTool upgrade that changes formatting still compares equal as long as both sides are read by the same binary.

## AGPL boundary

Immich and Ente solved related problems and their code is AGPL. Read them for ideas if you want. Do not copy, translate, or re-license that code into this repository. The same rule covers other AGPL photo libraries.
