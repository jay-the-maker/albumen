# MCP Bundle

`albumen-mcp.mcpb` is an [MCP Bundle](https://github.com/modelcontextprotocol/mcpb). The manifest is version 0.4. `.github/workflows/macos-binaries.yml` builds `albumen-mcp` for each platform, then runs `scripts/pack-mcpb.sh`. That script calls the official `@anthropic-ai/mcpb` 2.1.2 CLI: `mcpb validate`, then `mcpb pack`. The workflow filename stays `macos-binaries.yml` because the keyless cosign certificate names that file.

The bundle is a GitHub release asset:

https://github.com/jay-the-maker/albumen/releases/download/v0.1.1/albumen-mcp.mcpb

This repository does not publish the bundle to Smithery. Jay does that himself.

## What the default runs

The host asks for a library directory and starts the server with `--library` set to that directory. The manifest does not pass export or upload flags, and it does not pass `--apply` or `--execute`. Search and browse read the directory. The process does not write it. There is no delete tool.

On Linux and macOS the host runs `server/albumen-mcp`. That launcher execs the binary that matches `uname` and forwards the host's arguments unchanged. On Windows the host runs `server/albumen-mcp-win32-x86_64.exe`. MCPB platform overrides select an operating system, not an architecture, so the Windows command is the x86_64 executable. The aarch64 Windows executable is still in the bundle at `server/albumen-mcp-win32-aarch64.exe`.

| File | Machine |
| --- | --- |
| `server/albumen-mcp-linux-x86_64` | Linux x86_64 |
| `server/albumen-mcp-linux-aarch64` | Linux aarch64 |
| `server/albumen-mcp-darwin-arm64` | macOS arm64 |
| `server/albumen-mcp-darwin-x86_64` | macOS x86_64 |
| `server/albumen-mcp-win32-x86_64.exe` | Windows x86_64 (the host default) |
| `server/albumen-mcp-win32-aarch64.exe` | Windows aarch64 |

## Verify, then install

Download the bundle, its cosign bundle, and `SHA256SUMS` from the same release. `SHA256SUMS` lists `albumen-mcp.mcpb`. The macOS tarball has a different `SHA256SUMS` inside the archive. That inner file lists the three macOS binaries.

```bash
gh release download v0.1.1 --repo jay-the-maker/albumen \
  --pattern 'albumen-mcp.mcpb' \
  --pattern 'albumen-mcp.mcpb.bundle' \
  --pattern 'SHA256SUMS'
sha256sum -c SHA256SUMS
```

On macOS, `shasum -a 256 -c SHA256SUMS` checks the same file.

```bash
cosign verify-blob \
  --bundle albumen-mcp.mcpb.bundle \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/jay-the-maker/albumen/\.github/workflows/macos-binaries\.yml@.*' \
  albumen-mcp.mcpb

gh attestation verify albumen-mcp.mcpb --repo jay-the-maker/albumen
```

The checksum, the keyless cosign signature, and the GitHub attestation are the check. Open `albumen-mcp.mcpb` in a host that installs MCP Bundles after those three succeed. Point the library setting at the originals. This bundle does not turn export or upload on.

The workflow does not run `mcpb sign`. Signing is `cosign sign-blob --yes --bundle` and `actions/attest-build-provenance`. No paid account is required.

## How the release workflow builds it

Six runners build `albumen-mcp` with `cargo build --release --locked` and the pinned 1.99.1 toolchain: `ubuntu-24.04`, `ubuntu-24.04-arm`, `windows-2025`, `windows-11-arm`, `macos-14`, and `macos-15-intel`. An Ubuntu job downloads those binaries. `scripts/pack-mcpb.sh` refuses to pack unless every binary is present, the manifest version matches `mcp/Cargo.toml`, and the arguments are `--library` only. It writes `SHA256SUMS` beside the bundle. The job then signs `albumen-mcp.mcpb`, attests it, and uploads `albumen-mcp.mcpb`, `albumen-mcp.mcpb.bundle`, and `SHA256SUMS` with `gh release upload --clobber`.

A `v*` tag runs the workflow. `workflow_dispatch` uploads onto the release you name and does not move the tag. The default tag is `v0.1.1`. Pull requests run `scripts/pack-mcpb.sh --self-test` with stub binaries, so `mcpb validate` and `mcpb pack` run without a cross compile.
