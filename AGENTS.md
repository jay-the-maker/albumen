# AGENTS.md

Albumen: the open-source alternative to iCloud Photos, Google Photos and Frame.io.

This is the canonical guide for agents working in this repository. [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md) wins if the two disagree.

## Philosophy

Metadata and original quality are never lost. Originals stay bit-exact. EXIF, XMP, IPTC, GPS, sidecars, Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay. Edits are stored beside the original. Filenames and timestamps stay. Every copy is SHA-256 checked, and CI runs that check.

The default path never deletes a source. Leaving is one command: `albumen export`, then `albumen verify-manifest`. Storage is the user's. There are no dark patterns, upsell nags, or hidden scans. MCP is the agent interface, and agent instructions cannot add a tool the philosophy forbids.

The metrics are reliability, fidelity, respect, and control. Cost is outside those metrics.

## Where to start

Read [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md), then the crate you are changing.

| Change | Start here |
| --- | --- |
| Copy, checksum, export | `ingest/src/lib.rs`, `ingest/tests/roundtrip.rs` |
| `albumen export` / `verify-manifest` | `cli/` |
| Camera receiver | `receive/src/store.rs` |
| MCP tools | `mcp/src/tools.rs`, `mcp/src/main.rs` |
| iOS camera roll | `mobile/ios`, [docs/camera-roll-scope.md](docs/camera-roll-scope.md) |
| Android camera roll | `mobile/android`, [docs/camera-roll-scope.md](docs/camera-roll-scope.md) |
| What is not built | [ROADMAP.md](ROADMAP.md) |

Workspace members are `ingest` (`albumen-ingest`), `cli` (`albumen`), `mcp` (`albumen-mcp`), and `receive` (`albumen-receive`). Rust edition 2021, `rust-version` 1.94. CI uses toolchain 1.99.1.

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
export PATH="$(scripts/install-exiftool.sh):$PATH"
scripts/with-minio.sh cargo test --locked --all-targets
```

`scripts/with-minio.sh` installs the pinned MinIO binary and the pinned rclone, then runs the tests. ExifTool, MinIO, and rclone are pinned by version and SHA-256. Do not swap in a distro package.

Android, on a machine with a JDK:

```bash
scripts/android-ci.sh
```

iOS, on macOS:

```bash
swift test --package-path mobile/ios
```

A local real-device corpus is optional and must not be committed. See [CONTRIBUTING.md](CONTRIBUTING.md). CI does not set `ALBUMEN_REAL_CORPUS`.

## The round-trip gate

`ingest/tests/roundtrip.rs` copies the CC0 fixture corpus through the local filesystem, MinIO, and rclone (SFTP, WebDAV, and a MinIO stand-in for B2). It compares SHA-256, filename, mtime, and an ExifTool dump. The only ignored tags are filesystem identity tags a copy changes: directory, inode change time, access time, permissions, and the absolute path. `FileModifyDate`, `FileName`, and every embedded EXIF, XMP, IPTC, GPS, and QuickTime tag must match. Do not weaken that comparison.

## Never delete

- Do not add a CLI move, a receiver delete, or an MCP tool named delete, remove, unlink, move, or quarantine.
- `copy_verify_quarantine` is an explicit per-run opt-in. The CLI and the default MCP server do not call it, and source scans assert those crates do not name it. Leave that scan green.
- `BlobStore::delete` rolls back a destination this process just wrote when verification fails. It is not a user-facing delete.
- Do not `remove_file` a sidecar to replace it. Write a temp file in the same directory, fsync, rename over the old sidecar, and fsync the directory.
- The first `source_mtime` is write-once at commit. A later conflicting time is appended. It does not replace the first.
- Camera-roll backup leaves the item on the device. A copy that would strip GPS does not start.

## How to contribute

You are a contributor. Read [CONTRIBUTING.md](CONTRIBUTING.md). Every commit needs `Signed-off-by`. Keep a pull request to one concern. The pull request body checks the philosophy box and explains how the change upholds [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md). The `philosophy-ack` workflow fails unless that box is checked, the explanation is at least 20 characters and is not the template placeholder, and every commit in the pull request has `Signed-off-by`.

Do not copy Immich, Ente, or other AGPL photo-library source into this Apache-2.0 tree. MinIO is an external CI process and is not vendored. Do not commit real photos, real filenames, real checksums, or real GPS. Do not change branch protection. The only registry publish is `.github/workflows/publish-mcp.yml` on a `v*` tag. Do not publish [server.json](server.json) by hand, and do not submit the server to Glama, Smithery, PulseMCP, or mcp.so.
