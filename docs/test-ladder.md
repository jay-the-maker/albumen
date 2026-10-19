# Test ladder

Safety comes before a scan of a machine that holds the only copy. Each rung has to be green before the next one is a place to learn something. Sources are read-only, forever. A destination that resolves inside a source, or is the same path, is refused. There is no override. `copy_verify_quarantine` cannot target a source.

`chmod a-w` is not a substitute for a sandbox. It is an extra check inside rung 1.

## Rung 1 — CI, synthetic fixtures

Entry: a pull request, or a local `cargo test` of this tree.

What runs: `cargo fmt`, `cargo clippy`, and `scripts/with-minio.sh cargo test --locked --all-targets` on `ubuntu-24.04`, `ubuntu-26.04`, `macos-14`, and `windows-2025`. `windows-2025` is the pinned Windows image. It is not `windows-latest`. The corpus is the CC0 fixtures. `ALBUMEN_REAL_CORPUS` is unset in CI. Set it locally, and only locally, to a directory of copies plus `sanitized.sha256`. Do not commit that directory.

`scripts/check-dest-writes.py` fails the job if production code creates, writes, renames, or removes a file outside `ingest/src/dest.rs`. The bench harness is not part of that scan. It writes synthetic timing files and does not open a user archive.

Every `albumen` command, every MCP tool, and `albumen-receive` is run against a source tree twice: once writable, once after `chmod a-w`. The bytes, mtime, permissions, and xattrs must match afterwards. A destination inside the source must fail and must not create that directory.

Exit: those jobs are green, including the source snapshot tests. A red job stops the ladder.

## Rung 2 — disposable container

Entry: rung 1 is green. The host has rootless podman, or rootless docker. Rootful docker is refused. The script refuses to run as root.

`scripts/sandbox-trial.sh` copies the corpus (default `fixtures/`, or `--corpus DIR`) into a fresh directory. It never mounts the original path. The copy is mounted `:ro`. The manifest is written on a tmpfs. The container is `ubuntu:24.04` so its glibc matches the Ubuntu runner that built the binary. It is `--network none` and uses `scripts/seccomp-no-network.json`, which returns `EUNREACH` for socket calls. The script runs `albumen scan` and `albumen verify-manifest`.

The before/after snapshot is mtime, mode, and size. Access time is not part of it. Albumen's own reads use `O_NOATIME` on Linux, but ExifTool does not, and listing a directory can change that directory's access time. macOS has no `O_NOATIME`. Neither access time is restored.

`scripts/scan-no-network.sh` is the CI form of the same idea on Ubuntu. It straces `albumen scan` and an `albumen-receive` dry-run and fails if the trace contains a socket call or a write, unlink, rename, truncate, chmod, xattr set, or `utimensat` under the source. It then repeats the scan inside the seccomp container. ExifTool is a child of scan and has to succeed with sockets denied.

Exit: the script prints `rung 2 exit: source copy unchanged` and the metadata diff is empty.

## Rung 3 — generated million-item tree

Entry: rungs 1 and 2 are green.

`cli/tests/raid_scale.rs` is `#[ignore]` so an ordinary `cargo test`, and a pull request, does not create a million inodes. A million files hung the pull-request job, so [`.github/workflows/raid-scale.yml`](../.github/workflows/raid-scale.yml) runs it on `workflow_dispatch` and a weekly schedule. The job timeout is 180 minutes. The runner in that workflow is `macos-14` only. A Linux or Windows run of the same tree is not in the workflow. The command is:

```bash
cargo test --release --locked -p albumen --test raid_scale -- --ignored --nocapture --test-threads=1
```

The default tree has at least 1,000,000 wide zero-byte files, a deep directory that stays under `PATH_MAX`, mixed pairs and sidecars, unicode and odd filenames, a zero-byte file, and two 1 GiB sparse files. No real photos. `ALBUMEN_RAID_ITEMS` may shrink the wide set for a local smoke test. CI must not set it. `ALBUMEN_EXIFTOOL_BATCH` is 4096 for this run.

Scan writes the manifest outside the tree. Verify hashes the tree again. The before/after snapshot compares mtime, mode, size, and xattrs. Access time is not compared: ExifTool reads the files without `O_NOATIME`, and directory listings can change directory access time. Albumen does not write it back.

Exit: the ignored test passes on the scheduled or manual `macos-14` job and the snapshot diff is empty. A pull request does not wait for this rung.

## Rung 4 — owner Mac

Entry: rungs 1–3 are green, and the owner has said to run it. This rung is not a CI job. The procedure is [trial-macos.md](trial-macos.md).

Use a dedicated non-admin macOS user. Scan a copied folder, not the original volume. Take the network away with `scripts/macos-readonly-sandbox.sh`. Mount the copy read-only (`mount -u -o rdonly,noatime`, a read-only APFS snapshot, or a read-only disk image) so a read cannot change access time. macOS has no `O_NOATIME`. Without that mount, a read may update atime. Bytes, mtime, and xattrs are still left unchanged. `chmod a-w` does not replace the mount or the sandbox profile.

Exit: the mounted copy's bytes, mtime, xattrs, and access time match the snapshot taken before the scan. The original volume was not the path that was scanned.
