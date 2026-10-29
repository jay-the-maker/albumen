# Albumen

Albumen: the open-source alternative to iCloud Photos, Google Photos and Frame.io.

The commitment is [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md). Read it before changing this repository.

**First principle: metadata and original quality are never lost. Sources are read-only, forever.** A user's archive may be their only copy.

Originals are stored bit-exact: Albumen does not recompress, transcode, or strip them. EXIF, XMP, IPTC, and GPS stay. Sidecars stay. Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay. Edits are non-destructive and are stored alongside the originals. Timestamps and filenames are preserved. Every copy is checked with SHA-256 and fails loudly on mismatch. The default path never removes or alters a source. Scan, export, and upload read a source only through a read-only open. A destination inside that source is refused, and there is no override.

Albumen is an open-source photo library with the feel of classic iPhoto, built to keep working when the library is hundreds of thousands of items and multiple terabytes. You bring the storage. Agents can read the library; they do not get tools that delete or rewrite originals.

**Status: early-stage.** `albumen-mcp` is in this repository. It speaks MCP over stdio. Search, browse, and ledger are on, including metadata search (capture date, camera, lens, a GPS box, and XMP/IPTC keywords and captions). The ledger reads the library locally and lists every stored file, every on-device analysis, and every recorded destination. Export and upload are off until that process is started with `--enable-export` or `--enable-upload`. There is no delete tool. The native apps and the on-disk catalog are not built yet. Ingest copies an original plus its sidecars, preserves filename and mtime, and verifies SHA-256. It does not delete sources.

## Vision

Open the library and browse by time, place, and faces the way iPhoto did: quiet, fast, and obvious. The library underneath has to be boringly careful. A photo you took in 2009 is still that file in 2039, with the same bytes, the same name, and the same time and place.

Albumen is not a hosting service. Storage stays where you put it:

- Local disk, NAS, SMB, SFTP, WebDAV, S3, and Backblaze B2, reached directly or through rclone.
- Google Drive and Dropbox through the synced folder you already have. Albumen treats that folder as a filesystem.
- An iCloud Photos download, export, or Photos library copy imports into the library you choose, with those originals kept bit-exact.

Clients:

- macOS, SwiftUI and AppKit.
- iOS, Swift, with automatic camera-roll backup.
- Android, Kotlin, with automatic camera-roll backup.

The backup apps call the same ingest path as a drag-and-drop import. They do not recompress on the way in.

Linux, Windows, and macOS are first-class for the ingest tools. `albumen platform-dirs` prints the native config and data directories and creates nothing. The receiver can read its password from the Keychain, Windows Credential Manager, or the Secret Service, and it can write a launchd, systemd, or Task Scheduler definition. Each of those is opt-in. See [docs/platforms.md](docs/platforms.md).

## Quickstart

Four steps from a machine that does not have Rust. No sudo, and no account. The list is `scripts/quickstart.json`. A check fails if that list grows past five steps or if the copy is not SHA-256 verified.

1. Install the release binaries into a directory you own. The script downloads the archive for this operating system, requires the sibling `.sha256` to match, checks `SHA256SUMS` inside the archive, and checks a cosign bundle or a GitHub attestation when that tool is already on `PATH`.

   ```bash
   curl -fsSL https://raw.githubusercontent.com/jay-the-maker/albumen/main/scripts/install.sh | sh
   ```

   ```powershell
   irm https://raw.githubusercontent.com/jay-the-maker/albumen/main/scripts/install.ps1 | iex
   ```

   The binaries are `$HOME/.local/share/albumen/bin` on Linux and macOS, and `%LOCALAPPDATA%\albumen\bin` on Windows.

2. Start a local receiver. The password is one you choose for this receiver. It is not an Albumen account. The command creates the library directory.

   ```bash
   ALBUMEN_RECEIVE_PASSWORD='a receiver password' \
     "$HOME/.local/share/albumen/bin/albumen-receive" \
     --to "$HOME/albumen-library" --user camera --webdav 127.0.0.1:8080 --apply
   ```

3. PUT one original the way a phone does. The request sends Basic auth, `Content-Type: application/octet-stream`, and `X-Albumen-Mtime`. The source file is only read.

   ```bash
   curl -sS -D - -o /dev/null -T "$HOME/originals/IMG_QUICKSTART.jpg" \
     -u "camera:${ALBUMEN_RECEIVE_PASSWORD}" \
     -H "Content-Type: application/octet-stream" \
     -H "X-Albumen-Mtime: $(stat -c %Y "$HOME/originals/IMG_QUICKSTART.jpg" 2>/dev/null || stat -f %m "$HOME/originals/IMG_QUICKSTART.jpg")" \
     http://127.0.0.1:8080/IMG_QUICKSTART.jpg
   ```

4. Require the response `X-Albumen-Sha256`, the file in the library, and the source to be the same SHA-256. The source bytes and mtime stay as they were.

   ```bash
   sha256sum "$HOME/originals/IMG_QUICKSTART.jpg" "$HOME/albumen-library/IMG_QUICKSTART.jpg" \
     || shasum -a 256 "$HOME/originals/IMG_QUICKSTART.jpg" "$HOME/albumen-library/IMG_QUICKSTART.jpg"
   ```

Windows uses the same four steps. The commands are the `windows` fields in `scripts/quickstart.json`. `albumen-receive` answers `201` with `X-Albumen-Sha256` only after `copy_verify`. The source is not modified.

## Install from source

The toolchain in CI is Rust 1.99.1. The workspace asks for 1.94 or newer. Metadata search needs ExifTool on `PATH`. `scripts/install-exiftool.sh` downloads Image-ExifTool 13.59 and checks its SHA-256. It does not use a distro package.

```bash
git clone https://github.com/jay-the-maker/albumen.git
cd albumen
cargo build --release --locked
export PATH="$(scripts/install-exiftool.sh):$PATH"
./target/release/albumen-mcp --library ./library
./target/release/albumen export --from ./library --to /backup/library --apply
```

`albumen export` and `albumen-receive` print a plan and write nothing until `--apply` or `--execute`. `albumen scan` only reads. The safety ladder, the per-binary threat model, and how to check a release are in [docs/test-ladder.md](docs/test-ladder.md), [docs/threat-model.md](docs/threat-model.md), and [docs/release-verify.md](docs/release-verify.md).

That build produces `albumen-mcp`, `albumen`, and `albumen-receive`.

The container is the same server. The image entrypoint is `albumen-mcp --library /library`, and the image contains that pinned ExifTool. Search and browse only read the mounted directory. Append `--enable-export` or `--enable-upload` to turn those tools on. There is still no delete tool.

```bash
docker build -t albumen-mcp .
docker run --rm -i --mount type=bind,src="$PWD/library",dst=/library,readonly albumen-mcp
```

That mount is read-only. A writable directory for opt-in export or upload is a separate mount at `/destination`, not the library of originals. Tag `v0.1.1` publishes `ghcr.io/jay-the-maker/albumen-mcp:0.1.1` and the [server.json](server.json) entry `io.github.jay-the-maker/albumen` to the MCP Registry. [glama.json](glama.json) names the maintainer for Glama. This repository does not submit Glama, Smithery, PulseMCP, or mcp.so by itself.

## Try it

This walkthrough runs install, scan, export and verify in under five minutes. It uses only the synthetic files in `fixtures/` and writes only under `/tmp`, the checkout and `~/.cache/albumen`. It needs Rust 1.94 or newer and no account. Nothing is installed system-wide.

0. Install from source. ExifTool is unpacked under your user cache (`~/.cache/albumen`), not system-wide. On a clean checkout this took 91 s on an 8-core Linux box with the crates and the ExifTool tarball already downloaded. A first build on a slow network takes longer.

   ```bash
   git clone https://github.com/jay-the-maker/albumen.git && cd albumen
   cargo build --release --locked -p albumen
   export PATH="$(scripts/install-exiftool.sh):$PATH"
   ```

The remaining steps take a few seconds. Run them from the repository root.

1. Scan the fixtures. Scan only reads the source and writes a JSON Lines manifest elsewhere.

   ```bash
   ./target/release/albumen scan fixtures --manifest /tmp/albumen-scan.jsonl
   ```

   ```text
   albumen scan: 20 files, 101153 bytes hashed
   scanned 20 files (101153 bytes) into /tmp/albumen-scan.jsonl
   ```

2. Check the manifest against the fixtures.

   ```bash
   ./target/release/albumen verify-manifest --manifest /tmp/albumen-scan.jsonl --against fixtures
   ```

   ```text
   verified scan manifest against fixtures
   ```

3. Preview an export. Without `--apply` it prints the plan, writes nothing, and opens no network connection. `/tmp/albumen-export` still does not exist afterwards.

   ```bash
   ./target/release/albumen export --from fixtures --to /tmp/albumen-export
   ```

   ```text
   dry-run: no files written and no network connections opened. Pass --apply or --execute to copy.
   export
     from: fixtures
     to: /tmp/albumen-export
     manifest: /tmp/albumen-export/ALBUMEN-MANIFEST.txt
     network: none
     README.md
     ...
     files: 20
     bytes: 101153
   would export 20 new, 0 already present and verified
   ```

4. Run the export, then verify the copy against the manifest it wrote.

   ```bash
   ./target/release/albumen export --from fixtures --to /tmp/albumen-export --apply
   ./target/release/albumen verify-manifest --manifest /tmp/albumen-export/ALBUMEN-MANIFEST.txt --against /tmp/albumen-export
   ```

   ```text
   exported 20 new, 0 already present and verified
   to /tmp/albumen-export
   manifest /tmp/albumen-export/ALBUMEN-MANIFEST.txt
   verified 20 files in /tmp/albumen-export
   {"summary":true,"count_in":20,"count_out":20,"mismatched":0}
   ```

   Run the same `--apply` again and it prints `exported 0 new, 20 already present and verified`. Nothing is copied twice.

5. Check that the source did not change: `(cd fixtures && sha256sum -c --quiet SHA256SUMS)` prints nothing and exits 0. To clean up, remove `/tmp/albumen-scan.jsonl` and `/tmp/albumen-export`.

## Ingest core

`ingest/` is the `albumen-ingest` crate. The operation that matters is `copy_verify`:

1. Discover XMP and Apple AAE sidecars, plus RAW+JPEG and Live Photo pairs beside the file.
2. Copy the bytes. Do not open them with an image codec.
3. Keep the filename. Refuse a rename. Refuse to overwrite a file that is already there.
4. Keep the mtime. On S3, store it in object metadata (`albumen-mtime-ns`). `Last-Modified` is the upload time and is not the photo's timestamp.
5. Read the destination back, compute SHA-256, and compare. On mismatch, delete the destination copy and return an error that names both hashes. The source is left as it was.
6. If a later file in the asset fails, delete the destination copies this call wrote. Sources stay.

`copy_verify` is the only default path. There is no move in the CLI. `copy_verify_quarantine` cannot target a source: it returns an error and does not copy or rename. There is no flag that turns that off. Nothing in the CLI or the MCP server calls it.

```rust
use albumen_ingest::{copy_verify, discover, LocalStore};

let asset = discover("IMG_0001.jpg")?;
let report = copy_verify(&asset, &LocalStore::new("/library/originals"))?;
```

S3-compatible stores use `S3Store` (`ALBUMEN_S3_ENDPOINT`, `ALBUMEN_S3_BUCKET`, `ALBUMEN_S3_ACCESS_KEY`, `ALBUMEN_S3_SECRET_KEY`). The CI credentials are disposable test values, not secrets.

SFTP, WebDAV, and Backblaze B2 use `RcloneStore`. rclone is the transport and is not vendored. `put_verified` uploads, reads the bytes back, and checks SHA-256. The exact mtime is kept in a sibling `.{filename}.albumen-verify` object because those backends do not store nanoseconds. CI runs SFTP and WebDAV with `rclone serve` and points the B2 leg at MinIO.

## Export

`albumen export` copies a whole library, including sidecars and pairs, with `copy_verify`. The source is not modified. A file already at the destination with the same SHA-256 is left in place, so the command can be run again. A different file at that relative path is an error and is not overwritten.

```bash
albumen export --from ./library --to /backup/library
albumen verify-manifest --manifest /backup/library/ALBUMEN-MANIFEST.txt --against /backup/library

albumen export --from ./library --to s3://my-bucket/library --manifest ./library-manifest.txt
albumen verify-manifest --manifest ./library-manifest.txt --against s3://my-bucket/library
```

The manifest lists SHA-256, byte length, and the relative path. `verify-manifest` hashes the destination bytes and fails if a file is missing, extra, or different. S3 uses the same `ALBUMEN_S3_*` variables as ingest.

### Shell completions

`albumen completions <bash|zsh|fish|powershell|elvish>` prints a completion script to stdout and writes nothing else. Install it once per shell:

```bash
# bash
albumen completions bash > ~/.local/share/bash-completion/completions/albumen

# zsh (pick a directory on your $fpath)
albumen completions zsh > "${fpath[1]}/_albumen"

# fish
albumen completions fish > ~/.config/fish/completions/albumen.fish
```

```powershell
# PowerShell: add this line to your $PROFILE
albumen completions powershell | Out-String | Invoke-Expression
```

## Google Takeout

`albumen import-takeout` reads a Google Takeout download of `.zip` and `.tgz` parts, or an extracted folder, read-only. Each file is `copy_verify`'d. Takeout JSON is kept byte-for-byte next to its original. Capture time and GPS are restored into a new `.albumen.json` sidecar and are not written into the original. The reconciliation report names every unmatched file. Without `--apply` or `--execute` the command prints a plan and writes nothing. With `--apply` it exits non-zero unless the count in equals the count out and every SHA-256 matches.

```bash
albumen import-takeout --from ~/Downloads/takeout --to ~/library --report ~/takeout-report.jsonl --apply
```

The design note is [docs/importers/takeout.md](docs/importers/takeout.md).

## iCloud Photos

`albumen import-icloud` copies an iCloud Photos download, a Photos export, or a copy of a Photos library. A download may be several zip parts plus Photo Details CSVs. The source is opened read-only. Originals and edited versions stay bit-exact. Live Photo pairs and AAE/XMP sidecars are kept, and the CSVs are copied byte-for-byte. Album, favorite, caption, capture time, and GPS are exported beside the original. The database is opened in SQLite read-only immutable mode and is not modified. Without `--apply` or `--execute` the command prints a plan and writes nothing. With `--apply` it exits non-zero unless the count in equals the count out and every SHA-256 matches.

```bash
albumen import-icloud --from ~/Pictures/Photos\ Library.photoslibrary --to ~/library --report ~/icloud-report.jsonl --apply
```

The design note is [docs/importers/icloud.md](docs/importers/icloud.md).

## Read-only macOS trial

`albumen scan <dir> --manifest <out>` walks a library without opening it for write and records path, size, mtime, SHA-256, pairs, sidecars, and key metadata in a JSONL manifest. If the size or mtime changes while a file is read, that line also records `drift` with the before and after values, and the source file is left as it is. The manifest has to resolve outside the library. Run the same command again to resume. `albumen verify-manifest` re-hashes a scan manifest and reports drift. The exact commands for a subset of a large volume are in [docs/trial-macos.md](docs/trial-macos.md).

## Tests

The merge gate copies the fixture corpus through the local filesystem, through MinIO, and through rclone (SFTP, WebDAV, and a MinIO stand-in for B2), then compares SHA-256, filename, mtime, and an ExifTool metadata dump. Filesystem identity tags that any copy changes (directory, inode change time, access time, permissions, and the absolute path) are the only tags ignored. `FileModifyDate`, `FileName`, and every embedded EXIF, XMP, IPTC, GPS, and QuickTime tag must match.

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
scripts/with-minio.sh cargo test --locked
```

`scripts/install-exiftool.sh` downloads one ExifTool release and checks its SHA-256 before putting it on `PATH`. `scripts/install-rclone.sh` does the same for rclone. `scripts/with-minio.sh` does the same for the MinIO binary and installs that rclone. None of them follow a distro package.

Fixture provenance and the CC0 dedication are in [fixtures/README.md](fixtures/README.md). The architecture outline is in [docs/architecture.md](docs/architecture.md).

## Camera receiver

`albumen-receive` runs on your machine and listens for a camera. WebDAV and SFTP are the first choices. FTP is there for older bodies and does not encrypt. Each finished upload is stored with `copy_verify`. An existing file is left as it is. The receiver does not delete anything in the library.

```bash
albumen-receive --to ./library --user camera --password "$ALBUMEN_RECEIVE_PASSWORD" \
  --webdav 0.0.0.0:8080 --sftp 0.0.0.0:2222 --ftp 0.0.0.0:2121
```

The SFTP host key is generated for that process. FTP passive mode advertises the address of the control connection. A WebDAV 201 response includes `X-Albumen-Sha256`, the digest `copy_verify` checked. A different file that reuses a camera filename is stored under a free name. The camera filename and the uploaded directory are written to a sidecar. A 409 means those exact bytes are already stored. FTP `MFMT` and `MDTM`, SFTP attributes on the upload, and the WebDAV header `X-Albumen-Mtime` (Unix seconds, sent by the mobile clients) set the camera mtime while that file is being stored. The sidecar records that first `source_mtime` and a separate `received_at`. An identical retry, or a later `MFMT` or `setstat`, does not change them. A different time is appended as a conflict.

## Camera-roll backup

`mobile/ios` is a PhotoKit library, not an app. It exports originals, including a Live Photo's paired video, and does not ask PhotoKit for a rendered preview. An iCloud-optimized asset is fetched with network access allowed. A derivative is not uploaded in its place; that failure is logged. The receiver password is in the Keychain. The background-upload extension is scoped in [docs/camera-roll-scope.md](docs/camera-roll-scope.md).

`mobile/android/host` is the Android backup app. It opens MediaStore with `setRequireOriginal` and `ACCESS_MEDIA_LOCATION`, uploads to `albumen-receive` over WebDAV, and accepts the copy only when `X-Albumen-Sha256` matches the client's SHA-256. A mismatch is retried and shown. The first job runs on a network and is aimed at a verified copy within 5 minutes of setup. Later jobs run while the phone is idle, charging, and on a network. The receiver password is in Keystore-backed storage, not in WorkManager input. The camera-roll item stays. The debug APK is not published to a store. The path is [docs/mobile-android-backup.md](docs/mobile-android-backup.md).

## Agents

The agent guide is [AGENTS.md](AGENTS.md). `albumen-mcp` speaks MCP over stdio. Search, browse, and ledger are available immediately. Search can filter on capture date, camera, lens, a GPS box, and XMP/IPTC keywords and captions. It reads those from the files and sidecars and does not write a catalog. `albumen ledger` and the `ledger` tool print the same local account: path, SHA-256, size, member, the analyses that ran, and the destinations in the connection log. Export and upload are not available until that process is started with `--enable-export` or `--enable-upload`. Both write through `copy_verify` and do not overwrite. There is no delete tool.

```bash
albumen-mcp --library ./library
albumen-mcp --library ./library --destination ./destination --enable-export --enable-upload
```

`--destination` is required with those flags. It must be a directory that is not the library and not another mount of that same directory.

Details are in [docs/architecture.md](docs/architecture.md). What is not built yet is listed in [ROADMAP.md](ROADMAP.md).

## Support

Albumen will be funded through GitHub Sponsors (profile pending), and no other platforms.

## Apache-2.0 code in this tree

Albumen is Apache-2.0. Immich and Ente are useful AGPL photo libraries, and Albumen may share ideas with them. The code in this tree is Albumen's own. MinIO runs as a CI process and stays outside the tree.

## License

Apache-2.0. See [LICENSE](LICENSE). Contributions need a Developer Certificate of Origin sign-off. See [CONTRIBUTING.md](CONTRIBUTING.md).
