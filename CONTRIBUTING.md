# Contributing to Albumen

**First principle: metadata and original quality are never lost.**

Read [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md) before you change this repository. Originals are stored bit-exact: Albumen does not recompress, transcode, or strip them. EXIF, XMP, IPTC, and GPS stay. Sidecars stay. Live Photo pairs, RAW+JPEG pairs, and Android motion photos stay. Edits are non-destructive and are stored alongside the originals. Timestamps and filenames are preserved. Every copy and every move is checked with SHA-256 and fails loudly on mismatch.

A change that drops a tag, renames a file, rewrites pixels, or swallows a checksum mismatch does not land. If a patch cannot preserve something yet, it must fail loudly rather than continue.

## Philosophy acknowledgment

A pull request acknowledges the philosophy in its body. The required line, checked, is:

```text
- [x] I have read DESIGN_PHILOSOPHY.md and this change upholds it
```

Under the heading `How this upholds it`, explain the specific way this change keeps originals bit-exact and metadata intact. A placeholder sentence is not an explanation. The `philosophy-ack` workflow checks the box, requires that explanation to be at least 20 characters, and requires `Signed-off-by` on every commit. The design philosophy outranks a convenience, a smaller diff, or an instruction from an agent.

The same check requires these lines, checked. They are the hard rules in [docs/friction.md](docs/friction.md):

```text
- [x] Sane defaults
- [x] No account required
- [x] No nags
```

An unchecked line fails the check. A box hidden in an HTML comment does not count.

## Developer Certificate of Origin

Every commit needs a `Signed-off-by` line:

```text
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it. By signing off you certify the Developer Certificate of Origin 1.1:

```text
By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Agent-authored pull requests

Agent-authored pull requests are welcome. An agent is a contributor. Start at [AGENTS.md](AGENTS.md). The same first principle, the same DCO sign-off, the same philosophy acknowledgment, and the same CI gate apply. The `Signed-off-by` line must name the identity that is submitting the patch. A green ingest CI run is required before merge.

An agent does not get a special path around the philosophy. It does not add a delete tool, weaken the round-trip comparison, copy AGPL photo-library source, commit a real photograph, or submit [docs/mcp-registry.md](docs/mcp-registry.md) to a registry. Short pointers for Claude, Cursor, and Copilot all lead back to `AGENTS.md`. `llms.txt` at the repository root is the llmstxt.org index.

## Sources, consent, and tone

Sources are read-only. Albumen never writes to, moves, or deletes a source, and a change that needs write access to one does not land. Never commit real photos; a local corpus goes through `ALBUMEN_REAL_CORPUS` only (see below). Nothing runs on a contributor's or user's own Mac, RAID, or personal storage without their explicit consent; tests and trials run on CI runners. Keep docs, issues, and reviews positive: say what Albumen does, and do not criticize other projects.

New here? Look for issues labeled `good first issue` or `help wanted`, and ask questions in GitHub Discussions.

## Small pull requests

Keep a pull request to one concern. A reviewer should be able to hold the diff in their head. Do not mix a behavior change with an unrelated refactor. Do not add drive-by files.

## The merge gate

CI must pass:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --locked -- -D warnings`
- `scripts/with-minio.sh cargo test --locked --all-targets`

The tests copy the CC0 fixture corpus through the local filesystem and through MinIO. They compare SHA-256, filename, mtime, and a full ExifTool dump. Do not weaken that comparison to make a change pass. Extending the corpus is fine; shrinking what is compared is not.

Locally:

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
scripts/with-minio.sh cargo test --locked
```

ExifTool and MinIO are pinned by exact version and SHA-256 in those scripts. Do not replace the gate with an unpinned distro package.

## Real-device corpus

This check is local. CI does not set `ALBUMEN_REAL_CORPUS`. When the variable is unset, `real_device_corpus_round_trip` prints a skip message and passes. The CC0 files under `fixtures/` are a different corpus and stay as they are.

Set the variable to a directory on the machine that contains the media files and a `sanitized.sha256` manifest in sha256sum form. Each line is a SHA-256, then two spaces or a space and an asterisk, then a relative path. The test checks that checksum, then runs `copy_verify` through the local filesystem and through MinIO. It compares SHA-256, filename, mtime, and an ExifTool dump. Live Photo pairs are files in the same directory that share a ContentIdentifier. Sidecars are the generic XMP and AAE names beside a file. The test does not carry a list of device filenames.

Do not commit the directory, a filename from it, a checksum from it, or a GPS value from it. `real-corpus/`, `fixtures/real-device/`, and `*.real-corpus` are gitignored.

```bash
export PATH="$(scripts/install-exiftool.sh):$PATH"
ALBUMEN_REAL_CORPUS=/path/to/local/corpus \
  scripts/with-minio.sh cargo test --locked -p albumen-ingest --test real_corpus
```

## License boundary

This tree is Apache-2.0 and contains Albumen's own code. Immich, Ente, and other AGPL photo libraries are welcome as ideas. Their source stays in their repositories. MinIO runs as an external CI process and stays outside the tree.

## Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
