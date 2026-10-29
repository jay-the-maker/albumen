# Changelog

All notable changes to Albumen. Dates are UTC.

## Unreleased

Merged to `main` after v0.1.1, up to `4dad880`. These are in the next release.

### Added and changed

- Add albumen completions subcommand backed by clap_complete (#75)
- receive: dry-run-first, reversible receiver hide and unhide (CLI and route) (#202)
- Keep the scan ledger off the source volume: ALBUMEN_HOME or --ledger, never beside the source (#166) (#201)
- XO-52 soak plan: runnable script with a CI self-test on synthetic files (#166) (#197)
- Add albumen family sync: additive-only merge of two live libraries (#165)
- Snapshot copy B's full S3 object list and every file on copy A (#163)
- Make a skipped S3 two-copy test visible, and fail on Linux without MinIO (#162)
- Add a family tombstone that keeps every original (#155)
- Let family export update an existing copy, additively (#154)
- Keep a receiver hide on the copy where it happened (#152)
- Windows: flush directories through a write handle; a failed directory flush is an error (#144)
- Resolve source and destination folders once per import (#135)
- Preallocate, size and stage macOS destination writes until verified (#133)
- Name Android 14 Select photos and offer one Allow all back to a full backup (#131)
- Write import destinations without caching their pages on macOS (#124)
- Cut Windows import preflight CPU (#122)
- Keep Windows on per-worker copy_verify_file and record the disk class (#121)
- Read, hash, write, and read back on separate threads per import lane (#110)
- Keep the Android failure notification after the backup pass ends (#107)
- Lead the backup notification with the failure count and fit the body limit (#101)
- Search a saved path index at 380,000 items (#103)
- Add a local ledger of stored files, analyses, and destinations (#94)
- Install release binaries without Rust and verify the first backup (#88)
- Add a pairing QR for albumen-receive (#89)
- Wait for the failure on the status screen after permission(#87)
- Back up a phone into its own family namespace (#83)
- Add a one-step export of every family member's originals(#81)
- Refuse MCP write flags on the library mount (#70)
- Add the family-sharing namespace MVP (#74)
- Wait for the status file before reading a backup failure(#79)
- Publish Linux, Windows, and macOS release archives (#65)
- Make Linux, Windows, and macOS first-class ingest targets (#64)
- Pair Takeout JSON sidecars from discover(#67)
- Make scan and export take a read-only SourceRoot (#66)
- Add the iOS host and background upload extension (#62)
- Back up the Android camera roll with a verified WebDAV job (#59)
- Restore Takeout and iCloud metadata into sidecars without touching originals(#60)
- Keep sources read-only and gate writes behind an explicit apply(#48)
- Pair truncated Takeout JSON names and add optional derived XMP(#56)

### Tests, proofs and measurements

- soak: receiver library R from fixtures and a dry-run-first receive hide step (#203)
- Measure the import with sources on a separate volume (harness only, #138) (#189)
- Time the preflight sub-steps (timing only, #138) (#188)
- Pinned CC0 raw.pixls.us RAW corpus with a round-trip CI job (#175) (#185)
- Pinned CC0 HEIC, MOV and MP4 corpus with a round-trip CI job (#176) (#181)
- Pinned Wikimedia Commons corpus with a metadata round-trip CI job (#178)
- Test that a short copy fails the read-back and sources stay unchanged (#171)
- Prove two live family libraries merge without removing anything (#159)
- Prove a delete or tombstone on one copy never reaches the S3 copy (#158)
- Prove MCP hide and remove calls on one copy never touch the other (#157)
- Prove a family update leaves copy A byte-identical (#156)
- Prove a hide or revoke on one copy never removes the other copy (#148)
- Prove a delete never reaches the other backup copy (#137)
- Time import CPU per phase and cut preflight CPU (#118)
- Time the first thumbnail at 380,000 items and report import CPU (#106)
- Match import throughput to the same fsync and a median raw rate (#98)
- Prove scan, index, and search make no network connection (#92)
- Measure import throughput against a raw durable write (#93)
- Count phone backup steps from the pairing QR (#90)
- Count camera setup steps and time a 100 MB RAW verify(#91)
- Pin IPTC Core copies and a generated Apple ContentIdentifier (#85)
- Add a sharded nightly soak for phone backup faults (#86)
- Round-trip a synthetic CC0 Live Photo pair generated in CI (#84)
- Round-trip pinned CC0 HEIC and Apple ProRAW fixtures(#82)
- Measure backup recovery across injected receiver faults(#80)
- Prove the iOS backup on the simulator against a loopback receiver (#77)
- Assert the first verified backup and surfaced failures on the emulator(#68)
- Measure a 100,000-member Takeout import (#58)

### CI and performance gates

- CI: pin the Android jobs to Temurin JDK 21 via setup-java (#200)
- CI: print import-cpu-compare against the last main run, same disk class only (report only, #138) (#194)
- Drop unused actions: write from the backup scale soak workflow (#192)
- Print the disk class next to every Windows median (#191)
- Gate macOS on calibrated 3.0 with a 3.5 absolute backstop; print the runner image (#169)
- Drop the macOS absolute CPU backstop from 3.2 to 3.0 (#167)
- Gate macOS import CPU on a raw-rate calibrated best round at 3.0 (#164)
- Print the CPU gate with raw disk MB/s and SHA-pass ms on one line (#153)
- Gate the macOS best round at 3.2; every other leg stays at 3.0 (#150)
- Gate import CPU at 3.0 normalized on the best of 6 rounds; median gated off macOS (#125)
- Install a pinned, checksummed ffmpeg on Windows CI (#114)
- Set the macOS import target to 90% of the same-job copy ceiling (#108)
- Add a windows-2025 leg to the ingest CI matrix (#54)

### Docs

- docs: XO-52 real-device soak plan (#166), plan only (#195)
- docs: DCO remediation record for bae3003 (#191) (#193)
- Explain why the Commons round-trip leaves out Composite tags (#180)
- Document the two kinds of removal and mark Sync is not backup met on CI (#160)
- docs: Windows directory durability is a known issue until #139 (#142)
- Record the windows-2025 walk numbers from before the index (#105)
- Record the 380,000-item walk numbers from before the index (#104)
- Record loopback sends with a per-process kernel ETW session (#99)
- Document sync-is-not-backup and no account lockout(#78)
- Draft the safety ladder and the per-binary threat model (#72)
- Design a family library on storage the family already has (#63)
- Describe Albumen by what it keeps, in public docs(#61)

## Not yet released (queued, not on `main`)

These branches are waiting for review in the merge train (`docs/merge-queue.md`). None of them is in a release until it merges.

- docs/ci.md: the windows-2025 note (`issue-38-ci-doc-windows-2025`)
- README: a Try it section (`issue-47-readme-try-it`)
- README: a timed quickstart (`readme-quickstart-timed`)
- docs/compare.md: sourced, dated competitor comparison (`compare-doc`)
- Live Photo edge-case tests (`issue-45-live-photo-edge-tests`)
- albumen-receive front-end tests (`issue-46-receive-frontend-tests`)
- directory-sync counts at scale (`issue-138-dir-sync-scale`)
- scan and export scale harness (`scan-export-scale`)
- MCP stdio: newline-delimited JSON (`issue-mcp-stdio-ndjson`)
- MCP: tested against real clients (`mcp-real-clients`)
- MCP: thumbnails as image blocks (`mcp-thumbnail-image-block`)
- report photos whose GPS was stripped (`issue-25-gps-stripped`)
- export: `exported N new, M already present and verified` (`export-honest-rerun-count`)
- `verify-manifest --proof`: Markdown and JSON proof; tamper shows as drift (`verify-manifest-proof`)
- exit codes 0–4, a `next:` line on every error, one example per `--help` (`exit-codes`)
- export dry run: per-file present / conflict lines (`issue-40-dry-run-conflicts`)
- export to rclone remotes (`issue-23-rclone-export`)
- rclone SMB targets (`issue-22-rclone-smb`)
- `import-card`: camera card import (`issue-28-card-import`)
- `takeout-fidelity`: compare Takeout JSON with EXIF, read-only (`takeout-fidelity`)
- iCloud report format with ContentIdentifier (`icloud-fidelity`)
- export: list every conflict, `N conflicts, nothing written`, exit 3, dry run and `--apply` (`dry-run-all-conflicts`)
- end-to-end switch-from-Google test (`switch-from-google-e2e`)
- no-network and no-write strace audit for `takeout-fidelity` and `--proof`; import-card cleanup through dest.rs (`no-network-audit-fidelity-proof`)
- this changelog and the draft release notes (`changelog-release-notes`)

## v0.1.1

See the tag `v0.1.1`.

## v0.1.0

First tagged version. See the tag `v0.1.0`.
