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
| Family library | `albumen family` namespaces, signed manifest, invite, revoke, and per-viewer hide on a local directory or S3. [design/family-sharing.md](design/family-sharing.md). Rotation and sealing are still design |

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

`copy_verify` is the only default path. `copy_verify_quarantine` cannot target a source. It returns an error and does not copy or rename. There is no flag that turns that off. The CLI does not call it.

Android motion photos are one JPEG with an embedded video and XMP. They stay intact because the file is never rewritten. There is no separate pair to discover.

A stored original is not unlinked after that check. The two guarantees below say how, and where the tree is still short of them.

## Guarantees

### Sync is not backup

Deletes never propagate. Library content is append-only. A removal is a tombstone or a hide, and the stored original stays. A new file arrives through `copy_verify`. An existing stored original is left as it is.

The receiver refuses WebDAV `DELETE` and `MOVE`, SFTP remove and rename, and FTP `DELE`, `RMD`, and rename. A hide writes `{filename}.albumen-tombstone` beside the original. A second hide replaces that sidecar by rename. The original stays. When the receipt sidecar cannot be written after `copy_verify`, the same tombstone is written and the original stays. Both importers open the source read-only and `copy_verify` into the destination. The MCP server has no remove tool. `BlobStore::delete`, and an export rollback, remove only a destination this call just wrote, before that call has finished. The source is unchanged.

Receivers and importers are tested. The receiver round-trip sends `DELETE` and `DELE`, and the stored file stays. The receiver store tests a hide and a failed receipt. `cli/tests/deletes_never_propagate.rs` fails the build when the receiver, either importer, the CLI, or the MCP server unlinks a stored original.

Current gaps. A hide stays on the library where it was written. Nothing yet carries that hide to a second copy, and there is no sync. The delete-propagation test still to add uses two copies: a hide or a tombstone on the first, and the second copy's original bytes, name, and mtime stay. The test that runs today is a source scan, so it does not yet do that. A per-viewer hide is described in [design/family-sharing.md](design/family-sharing.md) and is not built. The library is a directory of ordinary files, so another program that can write the directory can unlink a file. Albumen's own commands leave the stored original in place.

### No account lockout

There is no vendor account. The library is plain files on storage the user already has: originals, sidecars, and tombstones in a directory. Those files are readable without Albumen. Keys and config stay on that machine. Export to a directory needs no service.

`albumen export --apply` copies the library to a directory. `albumen verify-manifest` checks that directory. Neither command contacts a service. The export is the same files. The receiver password is one the user chooses for that process. On a phone it is kept in the Keychain or in Keystore. S3 and rclone use the endpoint and keys the user already holds. Those values stay in the environment or in the local rclone config.

`cli/tests/offline_restore.rs` is the offline, no-account restore. It exports a synthetic library and runs `verify-manifest` with a cleared environment. On Linux the process runs with `connect()` blocked. The exported bytes match the library.

Current gaps. Signing keys for a shared library are described in the family design and are not built. In that design the private key stays in a local file, and reading a stored file does not require it. An `s3://` export uses the user's own endpoint and keys. The offline test covers a directory. The macOS library UI is not started, so the way to open the library today is the directory itself.

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

- macOS: SwiftUI for the library UI, and AppKit for import panels, drag and drop, and menus. It links or shells out to the ingest core. That UI is not started.
- iOS and Android: `mobile/ios` is the PhotoKit package plus the Xcode host and background upload extension. It writes `PHAssetResource` originals, including a Live Photo's paired video, with `isNetworkAccessAllowed` set so an iCloud original is downloaded. It does not call `PHImageManager.requestImage`, and it does not upload a derivative when the full original is missing. The background upload is [mobile-ios-backup.md](mobile-ios-backup.md). `mobile/android/host` is the Android backup app. It requests media and location permission and enqueues only when both are granted. CI does not click that dialog. Android opens MediaStore with `setRequireOriginal` and `ACCESS_MEDIA_LOCATION`. The first WorkManager job runs on a network and is aimed at a verified copy within 5 minutes of setup. Later jobs run while the device is idle, charging, and on a network. The receiver password is in Keystore-backed storage on Android and in the Keychain on iOS. WorkManager input carries the endpoint and the user only. Both PUT the bytes to `albumen-receive` and accept the copy only when `X-Albumen-Sha256` matches the client's SHA-256. A mismatch is retried and shown. The camera-roll original stays. The Android path is [mobile-android-backup.md](mobile-android-backup.md). The apps do not transcode to "save space" on the way in. The Android debug APK is not published to a store.

An iCloud Photos download, export, or Photos library copy imports into the library. Originals stay bit-exact in the directory you choose.

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

The macOS and iOS apps will be Swift, and the Android app will be Kotlin. The piece that must not lose a byte runs in CI on Linux and is callable from all three clients. The Rust library does that: explicit errors, streaming SHA-256, filesystem timestamps, and an S3 client, without a garbage-collected runtime in the copy path. The crate is small on purpose. UI, face detection, and the catalog stay outside it.

## CI

GitHub Actions runs `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` on `ubuntu-24.04` and `ubuntu-26.04`. A separate `ubuntu-24.04` job unit-tests the Android backup and assembles the debug APK. Another job runs the MediaStore instrumented tests on an API 30 emulator on `ubuntu-latest`, with KVM and `reactivecircus/android-emulator-runner`. That job checks that the first verified backup lands within 5 minutes of setup, and that a down receiver and a SHA-256 mismatch each show on the status screen and as a notification. A `macos-14` job builds the iOS package. Another `macos-14` job builds the iOS host and the background upload extension. A third `macos-14` job runs the simulator backup: a foreground upload to loopback `albumen-receive`, a byte-identical GPS EXIF segment, and a down receiver and a SHA-256 mismatch on the status store and as a local notification. That Xcode work does not run on Linux. The runner pin, the ExifTool 13.59 checksum, the MinIO checksum, and the Android SDK pin are in [ci.md](ci.md). MinIO is AGPL. The workflow downloads it and runs it; the Albumen tree does not contain MinIO source. The S3 credentials in CI are fixed test strings (`albumen` / `albumen-ci-not-a-secret`) for a server that exists only for that job.

The fixture corpus is synthetic and CC0. It includes a Motion Photo (JPEG plus appended MP4), an Apple `.aae` edit sidecar, and an Ultra HDR JPEG with a synthetic ICC profile and an MPF gain map, along with the earlier GPS, HEIC, RAW+JPEG, XMP, Live Photo, and video files. Tests compare each file with itself after a round trip. They do not depend on a checked-in ExifTool golden file, so an ExifTool upgrade that changes formatting still compares equal as long as both sides are read by the same binary.

## License boundary

This repository is Apache-2.0. Immich and Ente are useful AGPL photo libraries, and reading them for ideas is welcome. The code in this tree is Albumen's own. The same boundary covers other AGPL photo libraries: their source stays in their repositories.
