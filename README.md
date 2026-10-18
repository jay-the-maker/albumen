# Albumen

**First principle: metadata and original quality are never lost.**

Originals are stored bit-exact: Albumen does not recompress, transcode, or strip them. EXIF, XMP, IPTC, and GPS stay. Sidecars stay. Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay. Edits are non-destructive and are stored alongside the originals. Timestamps and filenames are preserved. Every copy and every move is checked with SHA-256 and fails loudly on mismatch.

Albumen is an open-source photo library with the feel of classic iPhoto, built to keep working when the library is hundreds of thousands of items and multiple terabytes. You bring the storage. Agents can read the library; they do not get tools that delete or rewrite originals.

**Status: early.** The native apps, the catalog, and the agent server are not built yet. This repository holds the charter and `albumen-ingest`, a small Rust library that copies an original plus its sidecars, preserves filename and mtime, and verifies SHA-256.

## Vision

Open the library and browse by time, place, and faces the way iPhoto did: quiet, fast, and obvious. The library underneath has to be boringly careful. A photo you took in 2009 is still that file in 2039, with the same bytes, the same name, and the same time and place.

Albumen is not a hosting service. Storage stays where you put it:

- Local disk, NAS, SMB, SFTP, WebDAV, S3, and Backblaze B2, reached directly or through rclone.
- Google Drive and Dropbox through the synced folder you already have. Albumen treats that folder as a filesystem.
- iCloud is import-only. Apple does not offer a stable API that can write originals back without rewriting them.

Clients:

- macOS, SwiftUI and AppKit.
- iOS, Swift, with automatic camera-roll backup.
- Android, Kotlin, with automatic camera-roll backup.

The backup apps call the same ingest path as a drag-and-drop import. They do not recompress on the way in.

## Ingest core

`ingest/` is the `albumen-ingest` crate. The operation that matters is `copy_verify`:

1. Discover XMP and Apple AAE sidecars, plus RAW+JPEG and Live Photo pairs beside the file.
2. Copy the bytes. Do not open them with an image codec.
3. Keep the filename. Refuse a rename. Refuse to overwrite a file that is already there.
4. Keep the mtime. On S3, store it in object metadata (`albumen-mtime-ns`). `Last-Modified` is the upload time and is not the photo's timestamp.
5. Read the destination back, compute SHA-256, and compare. On mismatch, delete the destination and return an error that names both hashes.

`move_verified` is that copy, and only then deletes the source.

```rust
use albumen_ingest::{copy_verify, discover, LocalStore};

let asset = discover("IMG_0001.jpg")?;
let report = copy_verify(&asset, &LocalStore::new("/library/originals"))?;
```

S3-compatible stores use `S3Store` (`ALBUMEN_S3_ENDPOINT`, `ALBUMEN_S3_BUCKET`, `ALBUMEN_S3_ACCESS_KEY`, `ALBUMEN_S3_SECRET_KEY`). The CI credentials are disposable test values, not secrets.

## Tests

The merge gate copies the fixture corpus through the local filesystem and through MinIO, then compares SHA-256, filename, mtime, and an ExifTool metadata dump. Filesystem identity tags that any copy changes (directory, inode change time, access time, permissions, and the absolute path) are the only tags ignored. `FileModifyDate`, `FileName`, and every embedded EXIF, XMP, IPTC, GPS, and QuickTime tag must match.

```bash
sudo apt-get install -y libimage-exiftool-perl
scripts/with-minio.sh cargo test --locked
```

Fixture provenance and the CC0 dedication are in [fixtures/README.md](fixtures/README.md). The architecture outline is in [docs/architecture.md](docs/architecture.md).

## Agents

The MCP server covers search, browse, export, and upload/ingest. Read tools are the default. Upload and ingest stay off until the user enables them, and both go through `copy_verify`: the call reports done only after the destination verifies, and it never overwrites an original. There is no delete tool. Details are in [docs/architecture.md](docs/architecture.md). What is not built yet is listed in [ROADMAP.md](ROADMAP.md).

## Support

Buy Me a Coffee: coming soon. No page exists yet.

## Not Immich, and not Ente

Immich and Ente are useful and they are AGPL. Albumen may share ideas. It does not copy their code, or the code of other AGPL photo libraries. MinIO, used only as a CI process, is also AGPL and is not vendored.

## License

Apache-2.0. See [LICENSE](LICENSE). Contributions need a Developer Certificate of Origin sign-off. See [CONTRIBUTING.md](CONTRIBUTING.md).
