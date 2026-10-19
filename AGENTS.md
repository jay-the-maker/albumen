# AGENTS.md

Albumen: the open-source alternative to iCloud Photos, Google Photos and Frame.io.

This is the canonical guide for agents working in this repository. [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md) wins if the two disagree.

## Philosophy

Metadata and original quality are never lost. Originals stay bit-exact. EXIF, XMP, IPTC, GPS, sidecars, Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay. Edits are stored beside the original. Filenames and timestamps stay. Every copy is SHA-256 checked, and CI runs that check.

The default path never deletes a source. Leaving is one command: `albumen export`, then `albumen verify-manifest`. Storage is the user's. There are no dark patterns, upsell nags, or hidden scans. MCP is the agent interface, and agent instructions cannot add a tool the philosophy forbids.

The metrics are reliability, fidelity, respect, control, scale, and the measured performance and friction budgets. Cost is outside those metrics. The numbers are in [docs/performance-budgets.md](docs/performance-budgets.md). The step count is in [docs/friction.md](docs/friction.md). Scale means no hard-coded ceiling on library size, file size, concurrency, or item counts: soft defaults the user can override, streaming and pagination, and constant memory as the library grows. The soft defaults and the safety bounds that stay are in [docs/scale.md](docs/scale.md).

## Where to start

Read [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md), then the crate you are changing.

| Change | Start here |
| --- | --- |
| Copy, checksum, export | `ingest/src/lib.rs`, `ingest/tests/roundtrip.rs` |
| Google Takeout import | `ingest/src/takeout.rs`, [docs/importers/takeout.md](docs/importers/takeout.md) |
| iCloud Photos import | `ingest/src/icloud.rs`, [docs/importers/icloud.md](docs/importers/icloud.md) |
| `albumen export` / `verify-manifest` | `cli/` |
| Read-only macOS trial, `albumen scan` | [docs/trial-macos.md](docs/trial-macos.md), `ingest/src/scan.rs` |
| Camera receiver | `receive/src/store.rs` |
| MCP tools | `mcp/src/tools.rs`, `mcp/src/main.rs` |
| iOS camera roll | `mobile/ios`, [docs/mobile-ios-backup.md](docs/mobile-ios-backup.md), [docs/camera-roll-scope.md](docs/camera-roll-scope.md) |
| Android camera roll | `mobile/android`, [docs/mobile-android-backup.md](docs/mobile-android-backup.md) |
| Benchmarks | `bench/`, [docs/performance-budgets.md](docs/performance-budgets.md) |
| Scale, page size, safety bounds | [docs/scale.md](docs/scale.md), `mcp/src/page.rs` |
| What is not built | [ROADMAP.md](ROADMAP.md) |

Workspace members are `ingest` (`albumen-ingest`), `cli` (`albumen`), `mcp` (`albumen-mcp`), `receive` (`albumen-receive`), and `bench` (`albumen-bench`). Rust edition 2021, `rust-version` 1.94. CI uses toolchain 1.99.1.

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

The performance job compares a scaled run with `bench/baseline.json`. It fails when the hashed-byte count, the copied-file count, or the search and browse file counts do not match that scale. Wall-clock is printed and is not a failure. Do not set `ALBUMEN_BENCH_ITEMS` in CI.

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
cargo run --release --locked -p albumen-bench -- --check
```

A 380,000-item run is local only: `ALBUMEN_BENCH_ITEMS=380000 cargo run --release --locked -p albumen-bench -- --measure`. That command prints JSON and does not compare it with the CI baseline.

## The round-trip gate

`ingest/tests/roundtrip.rs` copies the CC0 fixture corpus through the local filesystem, MinIO, and rclone (SFTP, WebDAV, and a MinIO stand-in for B2). It compares SHA-256, filename, mtime, and an ExifTool dump. The only ignored tags are filesystem identity tags a copy changes: directory, inode change time, access time, permissions, and the absolute path. `FileModifyDate`, `FileName`, and every embedded EXIF, XMP, IPTC, GPS, and QuickTime tag must match. Do not weaken that comparison.

## Never delete

- Do not add a CLI move, a receiver delete, or an MCP tool named delete, remove, unlink, move, or quarantine.
- `copy_verify_quarantine` cannot target a source. It returns an error and does not copy or rename. The CLI and the MCP server do not call it, and source scans assert those crates do not name it. Leave that scan green.
- A source is opened read-only. Production writes go through `ingest/src/dest.rs`. `scripts/check-dest-writes.py` fails the build otherwise. A destination inside a source is refused. There is no override.
- `export`, `upload`, and `albumen-receive` write only with `--apply` or `--execute`. Scan does not take that flag. The ladder is [docs/test-ladder.md](docs/test-ladder.md). The per-binary reach is [docs/threat-model.md](docs/threat-model.md).
- `BlobStore::delete` rolls back a destination this process just wrote when verification fails. It is not a user-facing delete.
- Do not `remove_file` a sidecar to replace it. Write a temp file in the same directory, fsync, rename over the old sidecar, and fsync the directory.
- The first `source_mtime` is write-once at commit. A later conflicting time is appended. It does not replace the first.
- Camera-roll backup leaves the item on the device. A copy that would strip GPS does not start.

## Sources, consent, and tone

- Sources are read-only, forever. Albumen never needs write access to a source: not in code, defaults, docs, or tests. A source may be someone's only copy. Writes go to the library or an export destination only.
- Never commit real photos. A local real-device corpus is used only through the `ALBUMEN_REAL_CORPUS` environment variable and stays on that machine. The CC0 files under `fixtures/` are the only media in the tree.
- Nothing runs on a user's Mac, RAID, or other personal storage without that person's explicit consent. Tests and trials, including [docs/trial-macos.md](docs/trial-macos.md), run on CI runners. A trial on a person's own machine happens only after they approve it.
- Keep the tone positive. Describe what Albumen does and how it does it. Do not criticize other products or projects in the repository, issues, or pull requests.

## How to contribute

You are a contributor. Read [CONTRIBUTING.md](CONTRIBUTING.md). Every commit needs `Signed-off-by`. Keep a pull request to one concern. The pull request body checks the philosophy box, checks `Sane defaults`, `No account required`, and `No nags`, and explains how the change upholds [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md). The `philosophy-ack` workflow fails unless those boxes are checked, the explanation is at least 20 characters and is not the template placeholder, and every commit in the pull request has `Signed-off-by`.

This tree is Apache-2.0 and contains Albumen's own code. AGPL photo-library source, including Immich and Ente, stays in those projects. MinIO is an external CI process and stays outside the tree. Do not commit real photos, real filenames, real checksums, or real GPS. Do not change branch protection. The only registry publish is `.github/workflows/publish-mcp.yml` on a `v*` tag. Do not publish [server.json](server.json) by hand, and do not submit the server to Glama, Smithery, PulseMCP, or mcp.so.
