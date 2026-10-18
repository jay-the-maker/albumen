# Read-only macOS trial

This is the command sequence for trying Albumen against a large photo volume on macOS arm64. The scan reads the volume. It does not open those files for write, and it does not change bytes, mtime, or xattrs. The manifest is a new file outside the volume.

The `v0.1.0` tag is unchanged. The macOS workflow uploads these binaries onto that existing GitHub release.

## Install the binaries

```bash
gh release download v0.1.0 --repo jay-the-maker/albumen --pattern 'albumen-macos-arm64*'
shasum -a 256 -c albumen-macos-arm64.tar.gz.sha256
mkdir -p "$HOME/albumen-trial"
tar -xzf albumen-macos-arm64.tar.gz -C "$HOME/albumen-trial"
cd "$HOME/albumen-trial"
shasum -a 256 -c SHA256SUMS
chmod +x albumen albumen-mcp
export PATH="$HOME/albumen-trial:$PATH"
```

ExifTool has to be on `PATH` before `scan` records key metadata. From a clone of this repository, install the pinned release (do not substitute a distro package):

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
```

## Scan a subset first

Pick a directory that is a real subset of the volume, then the volume root. Keep each manifest in your home directory, not on the volume.

```bash
albumen scan "/Volumes/RAID/Photos/2024" --manifest "$HOME/albumen-trial.jsonl"
albumen scan "/Volumes/RAID" --manifest "$HOME/albumen-raid.jsonl"
```

Progress is printed on stderr about once a second:

```text
albumen scan: 12000 files, 48318382080 bytes hashed
```

`albumen scan` refuses the run when `--manifest` resolves inside the source directory. Nothing in the source is opened for write. Files are opened with `O_RDONLY`. macOS has no `O_NOATIME`, so a read may update the access time. mtime, xattrs, and file bytes are not written.

Running the same command again continues after the last complete JSONL line. A partial last line is dropped and that file is scanned again. `ALBUMEN_SCAN_WORKERS` sets the hash threads. When it is unset, the process uses one worker per CPU. `ALBUMEN_EXIFTOOL_BATCH` sets how many files one ExifTool process reads (default 64).

## Check the subset

```bash
albumen verify-manifest --manifest "$HOME/albumen-trial.jsonl" --against "/Volumes/RAID/Photos/2024"
```

`verify-manifest` reads the source the same way and prints any missing path, extra path, SHA-256 mismatch, byte-length mismatch, or mtime mismatch. An empty report means the tree still matches the manifest. It does not compare metadata tags.
