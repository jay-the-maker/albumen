# Read-only macOS trial

This is rung 4 of [test-ladder.md](test-ladder.md). Rungs 1–3 run on CI. This page is the owner trial after those jobs are green, and only when the owner has said to run it. It is not a substitute for those jobs.

Use a dedicated non-admin macOS user. Work on a **copied** folder. Do not scan the original volume. Take the network away. Mount the copy read-only before the scan.

## Access time

macOS has no `O_NOATIME`. A read can update a file's access time. ExifTool's read can update it too, and a directory listing can update a directory's access time. Albumen does not write that time back, and it does not restore it. Bytes, mtime, and xattrs are what the scan leaves unchanged. Access time stays unchanged only when the copy is mounted so the kernel refuses the update.

Mount the copy read-only before `albumen scan`. Any one of these is enough:

- Update the mount that holds only the copy: `sudo mount -u -o rdonly,noatime` on that mount.
- Attach a read-only APFS snapshot of the volume that holds the copy.
- Put the copy in a disk image and attach that image read-only.

`chmod a-w` does not do this. It is not a substitute for a read-only mount or for `scripts/macos-readonly-sandbox.sh`.

## Account, copy, and network

Sign in as the dedicated non-admin user. The copy and the manifest both live under that user's home directory. Paths below are generic. They are not a live photo volume.

```bash
mkdir -p "$HOME/albumen-trial/copy" "$HOME/albumen-trial/mnt"
ditto "$HOME/photos-export" "$HOME/albumen-trial/copy"
```

`$HOME/photos-export` is the owner's copy, already off the original volume. `ditto` makes the folder this trial scans.

Put that folder on a read-only mount. A disk image:

```bash
hdiutil create -srcfolder "$HOME/albumen-trial/copy" \
  -format UDZO "$HOME/albumen-trial/copy.dmg"
hdiutil attach -readonly -nobrowse \
  -mountpoint "$HOME/albumen-trial/mnt" \
  "$HOME/albumen-trial/copy.dmg"
```

Or, when the copy is already its own mount, freeze that mount:

```bash
sudo mount -u -o rdonly,noatime "$HOME/albumen-trial/mnt"
```

An APFS snapshot of the volume that holds the copy, mounted read-only, is the same idea. Scan the mounted snapshot, not the writable folder.

The sandbox profile denies network and confines writes to the manifest's parent. It refuses a manifest that resolves inside the source.

```bash
scripts/macos-readonly-sandbox.sh \
  "$HOME/albumen-trial/mnt" \
  "$HOME/albumen-trial/scan.jsonl"
```

## Install the binaries

The `v0.1.0` tag is unchanged. The macOS workflow uploads these binaries onto that existing GitHub release. How to check the checksum and the keyless signature is [release-verify.md](release-verify.md). Notarization is deferred.

```bash
gh release download v0.1.0 --repo jay-the-maker/albumen --pattern 'albumen-macos-arm64*'
shasum -a 256 -c albumen-macos-arm64.tar.gz.sha256
mkdir -p "$HOME/albumen-trial"
tar -xzf albumen-macos-arm64.tar.gz -C "$HOME/albumen-trial"
cd "$HOME/albumen-trial"
shasum -a 256 -c SHA256SUMS
chmod +x albumen albumen-mcp albumen-receive
export PATH="$HOME/albumen-trial:$PATH"
```

ExifTool has to be on `PATH` before `scan` records key metadata. From a clone of this repository, install the pinned release (do not substitute a distro package):

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
```

## Scan the mounted copy

The manifest stays in the home directory, outside the mount.

```bash
albumen scan "$HOME/albumen-trial/mnt" --manifest "$HOME/albumen-trial/scan.jsonl"
```

Progress is printed on stderr about once a second:

```text
albumen scan: 12000 files, 48318382080 bytes hashed
```

`albumen scan` refuses the run when `--manifest` resolves inside the source directory. Nothing in the source is opened for write, and scan does not write a timestamp back onto a source. On Linux, files are opened with `O_RDONLY` and `O_NOATIME` when the kernel allows that flag. If the kernel refuses the flag, the open falls back to `O_RDONLY` and the scan continues. If the size or mtime changes while a file is being read, that JSONL line records `drift` with the before and after values (`bytes` and `mtime_ns`), and the source file is left as it is.

Running the same command again continues after the last complete JSONL line. A partial last line is dropped and that file is scanned again. `ALBUMEN_SCAN_WORKERS` sets the hash threads. When it is unset, the process uses one worker per CPU. `ALBUMEN_EXIFTOOL_BATCH` sets how many files one ExifTool process reads (default 64).

## Check the subset

```bash
albumen verify-manifest \
  --manifest "$HOME/albumen-trial/scan.jsonl" \
  --against "$HOME/albumen-trial/mnt"
```

`verify-manifest` reads the source the same way and prints any missing path, extra path, SHA-256 mismatch, byte-length mismatch, or mtime mismatch. An empty report means the tree still matches the manifest. It does not compare metadata tags. With the copy on a read-only, noatime mount, access time is unchanged as well.
