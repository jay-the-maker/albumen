# Test ladder

**Draft.** This page is the safety ladder as it stands. A row marked **planned** is not enforced yet. Do not treat a planned row as a green job.

Safety comes before a scan of a machine that holds the only copy. Each rung has to be green before the next one is a place to learn something. Sources are read-only, forever. A destination that resolves inside a source, or is the same path, is refused. There is no override. `copy_verify_quarantine` cannot target a source.

`chmod a-w` is not a substitute for a sandbox. It is an extra check inside rung 1.

## Rung 1 — CI, synthetic fixtures

Entry: a pull request, or a local `cargo test` of this tree.

What runs: `cargo fmt`, `cargo clippy`, and `scripts/with-minio.sh cargo test --locked --all-targets` on `ubuntu-24.04`, `ubuntu-26.04`, `macos-14`, `windows-2025`, and `windows-latest`. `windows-2025` is the pinned Windows image. `windows-latest` is the moving image beside it. The corpus is the CC0 fixtures. `ingest/tests/public_fixtures.rs` also downloads the two pinned CC0 files in `tests/fixtures/LICENSES.md` (a heic.fyi HEIC and a raw.pixls.us Apple ProRAW DNG) and round-trips those bytes. That test does not add a Live Photo pair or an IPTC block; the license file says why. `scripts/synthetic-live-photo.sh` generates a CC0 HEIC and MOV in the same job. The movie's identifier is `Keys:ContentIdentifier`. The still's identifier is `XMP-dc:Identifier`, because ExifTool cannot create Apple maker notes in that HEIC. The ingest and receiver tests round-trip that pair. The Windows legs also cover paths longer than 260 characters, case-insensitive name collisions, reserved device names, NTFS alternate data streams, and 100 ns modification times. The in-process scale tests, including the million-item page test that does not create a million inodes, run on every leg. `ALBUMEN_REAL_CORPUS` is unset in CI. Set it locally, and only locally, to a directory of copies plus `sanitized.sha256`. Do not commit that directory.

`scripts/check-dest-writes.py` fails the job if production code creates, writes, renames, or removes a file outside `ingest/src/dest.rs`. The bench harness is not part of that scan. It writes synthetic timing files and does not open a user archive.

Every `albumen` command, every MCP tool, and `albumen-receive` is run against a source tree twice: once writable, once after `chmod a-w`. The bytes, mtime, permissions, and xattrs must match afterwards. A destination inside the source must fail and must not create that directory.

Exit: those jobs are green, including the source snapshot tests. A red job stops the ladder.

## Rung 2 — disposable container

Entry: rung 1 is green. The host has rootless podman, or rootless docker. Rootful docker is refused. The script refuses to run as root.

`scripts/sandbox-trial.sh` copies the corpus (default `fixtures/`, or `--corpus DIR`) into a fresh directory. It never mounts the original path. The copy is mounted `:ro`. The manifest is written on a tmpfs. The container is `ubuntu:24.04` so its glibc matches the Ubuntu runner that built the binary. It is `--network none` and uses `scripts/seccomp-no-network.json`, which returns `EUNREACH` for socket calls. The script runs `albumen scan` and `albumen verify-manifest`.

The before/after snapshot is mtime, mode, and size. Access time is not part of it. Albumen's own reads use `O_NOATIME` on Linux, but ExifTool does not, and listing a directory can change that directory's access time. macOS has no `O_NOATIME`. Neither access time is restored.

`scripts/scan-no-network.sh` is the CI form of the same idea on Ubuntu. It straces `albumen scan` and an `albumen-receive` dry-run and fails if the trace contains a socket call or a write, unlink, rename, truncate, chmod, xattr set, or `utimensat` under the source. It then repeats the scan inside the seccomp container. ExifTool is a child of scan and has to succeed with sockets denied.

`scripts/no-unexpected-egress.sh` runs each binary in a user network namespace. The namespace has loopback and no route beyond it. The trace may show a connection only when the command was given that destination: `ALBUMEN_S3_ENDPOINT` for an S3 export, or the listen address for `albumen-receive --apply`. Every other address fails the run.

Exit: the script prints `rung 2 exit: source copy unchanged` and the metadata diff is empty.

## Rung 3 — generated million-item tree on macOS, Windows, and Linux

Entry: rungs 1 and 2 are green.

`cli/tests/raid_scale.rs` is `#[ignore]` so an ordinary `cargo test`, and a pull request, does not create a million inodes. A million files hung the pull-request job, so [`.github/workflows/raid-scale.yml`](../.github/workflows/raid-scale.yml) runs it on `workflow_dispatch` and a weekly schedule. The job timeout is 180 minutes. The runner today is `macos-14` only. The command is:

```bash
cargo test --release --locked -p albumen --test raid_scale -- --ignored --nocapture --test-threads=1
```

The default tree has at least 1,000,000 wide zero-byte files, a deep directory that stays under `PATH_MAX`, mixed pairs and sidecars, unicode and odd filenames, a zero-byte file, and two 1 GiB sparse files. No real photos. `ALBUMEN_RAID_ITEMS` may shrink the wide set for a local smoke test. CI must not set it. `ALBUMEN_EXIFTOOL_BATCH` is 4096 for this run.

Scan writes the manifest outside the tree. Verify hashes the tree again. The before/after snapshot compares mtime, mode, size, and xattrs. Access time is not compared: ExifTool reads the files without `O_NOATIME`, and directory listings can change directory access time. Albumen does not write it back.

Exit: the ignored test passes on the scheduled or manual `macos-14` job and the snapshot diff is empty. A pull request does not wait for this rung.

**Planned:** the same generated tree on a Linux runner and on a Windows runner. Those jobs are not in the workflow. Until they are, rung 3 is enforced on macOS only.

## Real-device backup protocol — waiting on Jay's OK, do not run

**Waiting on Jay's OK. Do not run.** This is not a CI job, not a pull-request check, and not the next rung. Nothing in this section runs until Jay approves it in writing.

No Jay phone, no Jay Mac, and no Jay RAID is touched until that approval. The machines in the steps below are not those machines. CI runners and the synthetic libraries in this repository are the only places the workflows run.

First principle: there is no write path to originals, to a source, or to another member's namespace. The phone's camera roll stays where it is. A backup reads it and writes the copy only into that member's own prefix, `members/<that-member-ed25519-pubkey>/`, on storage that is not the source. A receiver or a test harness that would write anywhere else does not start. Bytes, mtime, and sidecars of every other member are compared by SHA-256 before and after and have to match. A source is opened read-only. Leaving the library is `albumen export`, then `albumen verify-manifest`. There is no delete.

Devices, after approval, and only devices that are not Jay's:

- One Android phone, charging, on Wi-Fi, with a camera roll of synthetic items or of copies the owner of that phone made for this run.
- One iPhone, charging, on Wi-Fi, with the same kind of roll.
- One receiver on disk that is not a Jay Mac and not a Jay RAID. The library is new, or it is a family library whose other members were seeded with synthetic objects before the run.

Duration: 24 hours or more of wall clock. The clock starts when the first backup is armed and stops only after that window, including time the phone is asleep.

Faults, injected on a schedule that is seeded and written down before the window starts: network drop (the path between the phone and the receiver closes during a PUT), power loss (the phone reboots, or the backup process is killed, during an upload), receiver down (the receiver is not listening), and a SHA-256 mismatch (the receiver answers 201 with a digest that is not the bytes it was sent). Each fault is logged with the seed, the item name, and the time. The same seed has to reproduce the schedule.

What is measured: verified percent against elapsed time, the time at which verified items first reach 99.9 percent, and silent failures. A silent failure is an item that was attempted, is still unverified, and whose filename is absent from the status file and from the backup notification. The run fails if that count is not zero. Original bytes, mtime, and embedded metadata on the phone are compared with the copy that verified. Other members' originals, mtime, and sidecars are byte-identical by SHA-256.

The scheduled CI soak is not this protocol. It does not last 24 hours, it does not reboot a phone, and it does not open a real camera roll.

## Rung 4 — last rung, owner Mac

The procedure is [trial-macos.md](trial-macos.md).

Gated: run on an owner's machine only after rungs 1-3 are green and with the owner's explicit approval. CI macOS runners are the default place this runs.

Use a dedicated non-admin user. Scan a copied folder, not the original volume. Take the network away with `scripts/macos-readonly-sandbox.sh`. Mount the copy read-only (`mount -u -o rdonly,noatime`, a read-only APFS snapshot, or a read-only disk image) so a read cannot change access time. macOS has no `O_NOATIME`. Without that mount, a read may update atime. Bytes, mtime, and xattrs are still left unchanged. `chmod a-w` does not replace the mount or the sandbox profile.

Exit: the mounted copy's bytes, mtime, xattrs, and access time match the snapshot taken before the scan. The original volume was not the path that was scanned.

## Rung 5 — Windows sandbox

Entry: rungs 1–4 are green. This rung runs on a Windows machine. It is not a GitHub-hosted job.

Use Windows Sandbox, or a container with the network off. Windows Sandbox is a disposable desktop. The configuration [scripts/windows-sandbox.wsb](../scripts/windows-sandbox.wsb) sets `<Networking>Disable</Networking>` and maps a copied folder read-only. `scripts/windows-sandbox.ps1` copies `fixtures/` (or `-Corpus`) into a fresh directory and writes that configuration next to the copy. It does not mount the original path.

Inside the sandbox, run `albumen scan` and `albumen verify-manifest` against the mapped copy. The manifest is written outside the mapped folder. Compare the copy's bytes, modification time, and alternate data streams with the snapshot taken before the scan. A container with `--network none` is the same rung when Windows Sandbox is not available.

Exit: the script prints `rung 5 exit: source copy unchanged`. The live library was not the path that was scanned.
