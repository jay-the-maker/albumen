# Read-only macOS trial

This is the command sequence for trying Albumen against a **copy** of a photo folder on macOS arm64. It is rung 4 of [test-ladder.md](test-ladder.md). Rungs 1–3 have to be green first. The live volume is not the first path you scan.

The scan reads the copy. It does not open those files for write, and it does not change bytes, mtime, or xattrs. The manifest is a new file outside the copy. `chmod a-w` is not a substitute for `scripts/macos-readonly-sandbox.sh`, which denies network and confines writes to the manifest's parent.

The `v0.1.0` tag is unchanged. The macOS workflow uploads these binaries onto that existing GitHub release. How to check the checksum and the keyless signature is [release-verify.md](release-verify.md). Notarization is deferred.

## Install the binaries

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

## Scan a copy first

Copy a subset. Keep the manifest in your home directory, not inside the copy and not on the live volume.

```bash
ditto "/Volumes/RAID/Photos/2024" "$HOME/albumen-trial/copy"
scripts/macos-readonly-sandbox.sh \
  "$HOME/albumen-trial/copy" \
  "$HOME/albumen-trial/scan.jsonl"
```

The same scan without the sandbox profile, still aimed at the copy:

```bash
albumen scan "$HOME/albumen-trial/copy" --manifest "$HOME/albumen-trial/scan.jsonl"
```

Progress is printed on stderr about once a second:

```text
albumen scan: 12000 files, 48318382080 bytes hashed
```

`albumen scan` refuses the run when `--manifest` resolves inside the source directory. Nothing in the source is opened for write, and scan does not write a timestamp back onto a source. On Linux, files are opened with `O_RDONLY` and `O_NOATIME` when the kernel allows that flag. If the kernel refuses the flag, the open falls back to `O_RDONLY` and the scan continues. macOS has no `O_NOATIME`, so a read may update the access time. ExifTool's own read can update the access time on any platform, and scan leaves that time alone. mtime, xattrs, and file bytes are not written. If the size or mtime changes while a file is being read, that JSONL line records `drift` with the before and after values (`bytes` and `mtime_ns`), and the source file is left as it is.

Running the same command again continues after the last complete JSONL line. A partial last line is dropped and that file is scanned again. `ALBUMEN_SCAN_WORKERS` sets the hash threads. When it is unset, the process uses one worker per CPU. `ALBUMEN_EXIFTOOL_BATCH` sets how many files one ExifTool process reads (default 64).

## Check the subset

```bash
albumen verify-manifest --manifest "$HOME/albumen-trial/scan.jsonl" --against "$HOME/albumen-trial/copy"
```

`verify-manifest` reads the source the same way and prints any missing path, extra path, SHA-256 mismatch, byte-length mismatch, or mtime mismatch. An empty report means the tree still matches the manifest. It does not compare metadata tags.
