# Verifying a macOS release

`.github/workflows/macos-binaries.yml` builds `albumen`, `albumen-mcp`, and `albumen-receive` on `macos-14` from CI only. It uses `cargo build --release --locked` with the pinned 1.99.1 toolchain. `SOURCE_DATE_EPOCH` is the commit time. `RUSTFLAGS` remaps the workspace path to `/albumen`. Incremental compilation is off and the release profile strips symbols.

That is reproducible-ish. It is not a claim that a second machine will produce bit-identical Mach-O files. The macOS linker still embeds non-deterministic content. Check the checksum file from the same CI run, and check the signature. Do not expect a local rebuild to hash-match the artifact.

The workflow writes `SHA256SUMS` inside `albumen-macos-arm64.tar.gz`, and `albumen-macos-arm64.tar.gz.sha256` beside it. It signs each binary and the tarball with keyless cosign (`cosign sign-blob --yes --bundle`) and attests them with `actions/attest-build-provenance`. A `v*` tag runs the workflow. `workflow_dispatch` uploads onto an existing release and does not move the tag.

```bash
gh release download v0.1.0 --repo jay-the-maker/albumen --pattern 'albumen-macos-arm64*'
shasum -a 256 -c albumen-macos-arm64.tar.gz.sha256
tar -xzf albumen-macos-arm64.tar.gz
shasum -a 256 -c SHA256SUMS
```

Download the `.bundle` files from the same release, then:

```bash
cosign verify-blob \
  --bundle albumen.bundle \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/jay-the-maker/albumen/\.github/workflows/macos-binaries\.yml@.*' \
  albumen

gh attestation verify albumen-macos-arm64.tar.gz --repo jay-the-maker/albumen
```

Repeat `cosign verify-blob` for `albumen-mcp`, `albumen-receive`, and `albumen-macos-arm64.tar.gz`.

The MCP Bundle on the same workflow is `albumen-mcp.mcpb`. Its checksum, keyless signature, and GitHub attestation are in [mcpb.md](mcpb.md). The release asset `SHA256SUMS` lists that bundle. The `SHA256SUMS` inside `albumen-macos-arm64.tar.gz` lists the three macOS binaries.

## Gatekeeper

Release binaries are checked with the checksum and the keyless signature. Notarization needs a paid Apple Developer Program membership. Gatekeeper will warn when you open the binary. That warning is expected.

Do not disable Gatekeeper. `spctl --master-disable` is not a setup step. Verify the checksum and the keyless signature, then decide whether to run the binary. The owner trial in [trial-macos.md](trial-macos.md) stays on a copied folder after the [test ladder](test-ladder.md) is green.
