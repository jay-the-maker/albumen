# Threat model

Sources are read-only, forever. A user's archive may be their only copy. `SourceRoot` canonicalizes the directory it was given and opens files with `O_RDONLY`. On Linux it also sets `O_NOATIME` when the kernel allows it. The type has no create, write, rename, unlink, utimens, chmod, or xattr-set operation.

Scan, ingest, and export read originals through that type. A destination that resolves inside a source root, or is the same path, is refused after canonicalization and symlink resolution. Where a hard link can be seen, a destination that is a hard link to an inode inside the source is refused too. There is no flag that turns the refusal off.

`copy_verify_quarantine` returns an error immediately. It does not copy and it does not rename. The CLI and the MCP server do not call it.

Production code creates, writes, renames, and removes files only in `ingest/src/dest.rs`. `scripts/check-dest-writes.py` enforces that. The bench harness is outside the scan because it writes synthetic files for timing and does not open a user archive.

Commands that write (`albumen export`, MCP `export` and `upload`, `albumen-receive`) do nothing without `--apply` or `--execute`. They print a plan. `albumen scan` is read-only by construction and does not take that flag. An illegal destination is an error on the dry-run path too. It is not printed as a plan.

There is no `albumen ingest` subcommand. The write that stores bytes from a library is `export --apply`. The write that stores an external file into a library is MCP `upload` with `--apply`. The receiver's library is a destination, not a source archive.

## albumen

| | Default | Opt-in |
| --- | --- | --- |
| Read | The `--from` directory for `export`, the positional directory for `scan`, and `--against` for `verify-manifest`. Opens are `O_RDONLY`. | An `s3://` destination is not contacted unless `--apply` or `--execute` is set. |
| Write | Nothing. `export` prints the plan. `scan` is refused when the manifest resolves inside the source, so a refused scan creates nothing. | `--apply` or `--execute` on `export` writes the destination and the manifest, both outside the source. `scan` writes the manifest outside the source. `verify-manifest` writes nothing. |
| Network | None. A dry-run that names `s3://` says that a later apply would connect. It does not connect. | `export --apply` to `s3://` connects to `ALBUMEN_S3_ENDPOINT`. `verify-manifest` of an export manifest against `s3://` reads objects there. A scan manifest is verified against a local directory only. |

CI straces `scan` and fails if it opens a socket or writes under the source. The same scan is repeated with a seccomp profile that denies socket calls. `scripts/no-unexpected-egress.sh` runs every `albumen` command that reads a library, including export and both importers, inside a network namespace and fails if a connection is not the destination that command was given.

## albumen-mcp

| | Default | Opt-in |
| --- | --- | --- |
| Read | The `--library` directory, through `search` and `browse`, and through metadata reads of those files and their sidecars. | `export` reads that same library. `upload` reads the file path the caller passed. That path is the source. The library is the destination. |
| Write | Nothing. `export` and `upload` are not offered unless `--enable-export` or `--enable-upload` is set. Even then, without `--apply` or `--execute`, they return a JSON plan and do not create directories or connect to S3. | With the enable flag and `--apply` or `--execute`, `export` writes a destination outside the library. `upload` writes into the library from a file whose parent is not that library. A destination inside the source is refused. |
| Network | None. stdio only. | `export --apply` to `s3://` connects to `ALBUMEN_S3_ENDPOINT`. Nothing else dials out. |

There is no delete, move, or quarantine tool. Inbound messages are capped by `ALBUMEN_MCP_MAX_MESSAGE` (default 8 MiB) before the body is allocated. The same egress script drives a search, a dry-run export, and an export that was given `ALBUMEN_S3_ENDPOINT`. Only that endpoint may appear in the trace.

## albumen-receive

| | Default | Opt-in |
| --- | --- | --- |
| Read | Nothing from a photo archive. A dry-run does not open the library. | `--apply` or `--execute` reads the sockets it is listening on, and reads the spool it just wrote. |
| Write | Nothing. The plan names `--to` and does not create it. The password is printed as `set` or `unset`, not as the secret. | `--apply` creates `--to` if needed and writes finished uploads through `copy_verify`. A mismatch deletes the destination just written. There is no delete of a stored original. Sidecar replacement writes a temp file in the library and renames it over the previous sidecar. |
| Network | None. No socket is opened. | Listens on the `--webdav`, `--sftp`, and `--ftp` addresses that were passed. At least one is required. FTP is cleartext. SFTP host key is in memory. There is no outbound connection. |

`--to` is a destination library. It is not a `SourceRoot`. Do not point it at an archive that is the only copy. The dry-run identity test points `--to` at a fixture and requires that fixture to stay unchanged. `--apply` in that test listens on a different directory.

## Network egress

`scripts/no-unexpected-egress.sh` runs `albumen`, `albumen-mcp`, and `albumen-receive` in a user network namespace. The namespace has loopback and no route beyond it. `strace` logs every `connect`, `sendto`, `sendmsg`, and `bind`. `scripts/check-egress.py` fails the run when a logged address is not a destination that invocation named.

A scan, a local export, a Takeout or iCloud dry-run, `platform-dirs`, an MCP search, and a receive dry-run name no network destination. Any Internet address fails that run. `export --apply` to `s3://` may connect to `ALBUMEN_S3_ENDPOINT` and nowhere else. `albumen-receive --apply` may bind the listen address it was given. It does not connect out. The CI job is the `sandbox` job on `ubuntu-24.04`.

## What this does not claim

Apple notarization is not done. Gatekeeper will warn on the macOS binaries. The gap is recorded in [release-verify.md](release-verify.md). The ladder in [test-ladder.md](test-ladder.md) is how a real machine is approached. Rung 4 is a copied folder after rungs 1–3 are green.
