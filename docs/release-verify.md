# Verifying a release

`.github/workflows/macos-binaries.yml` builds `albumen`, `albumen-mcp`, and `albumen-receive` in CI. The filename stays `macos-binaries.yml` so the keyless certificate identity stays the same path. Each archive is a native build:

| Archive | Runner | Target |
| --- | --- | --- |
| `albumen-linux-x86_64.tar.gz` | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` |
| `albumen-linux-aarch64.tar.gz` | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` |
| `albumen-windows-x86_64.tar.gz` | `windows-latest` | `x86_64-pc-windows-msvc` |
| `albumen-windows-aarch64.tar.gz` | `windows-11-arm` | `aarch64-pc-windows-msvc` |
| `albumen-macos-arm64.tar.gz` | `macos-14` | `aarch64-apple-darwin` |
| `albumen-macos-x86_64.tar.gz` | `macos-15-intel` | `x86_64-apple-darwin` |

Linux archives are gnu. Windows archives are MSVC, and the Windows binaries inside the archive are `albumen.exe`, `albumen-mcp.exe`, and `albumen-receive.exe`. macOS publishes both architectures. The macOS arm64 archive name is unchanged.

The build uses `cargo build --release --locked --target <triple>` with the pinned 1.99.1 toolchain. `SOURCE_DATE_EPOCH` is the commit time. `RUSTFLAGS` remaps the workspace path to `/albumen`. Incremental compilation is off and the release profile strips symbols.

That is reproducible-ish. It is not a claim that a second machine will produce bit-identical binaries. Check the checksum file from the same CI run, and check the signature.

Each archive contains `SHA256SUMS` for the three binaries. `<archive>.sha256` sits beside the archive. The workflow signs each binary and the archive with keyless cosign (`cosign sign-blob --yes --bundle`). Bundle names are `<archive-stem>.<file>.bundle`, for example `albumen-macos-arm64.albumen.bundle` and `albumen-macos-arm64.albumen-macos-arm64.tar.gz.bundle`. It attests the three binaries and the archive with `actions/attest-build-provenance`. A `v*` tag runs the workflow. `workflow_dispatch` uploads onto an existing release and does not move the tag.

```bash
gh release download v0.1.0 --repo jay-the-maker/albumen --pattern 'albumen-macos-arm64*'
shasum -a 256 -c albumen-macos-arm64.tar.gz.sha256
tar -xzf albumen-macos-arm64.tar.gz
shasum -a 256 -c SHA256SUMS
```

`sha256sum -c` checks the same files on Linux and Windows. Download the `.bundle` files from the same release, then:

```bash
cosign verify-blob \
  --bundle albumen-macos-arm64.albumen.bundle \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/jay-the-maker/albumen/\.github/workflows/macos-binaries\.yml@.*' \
  albumen

gh attestation verify albumen-macos-arm64.tar.gz --repo jay-the-maker/albumen
```

Repeat `cosign verify-blob` for `albumen-mcp`, `albumen-receive`, and `albumen-macos-arm64.tar.gz`, using the bundle whose name starts with `albumen-macos-arm64.`. The same commands apply to the other five archives. On Windows the binary names end in `.exe`.

## Gatekeeper

Release binaries are checked with the checksum and the keyless signature. Notarization needs a paid Apple Developer Program membership. Gatekeeper will warn when you open the binary. That warning is expected.

Do not disable Gatekeeper. `spctl --master-disable` is not a setup step. Verify the checksum and the keyless signature, then decide whether to run the binary. The owner trial in [trial-macos.md](trial-macos.md) stays on a copied folder after the [test ladder](test-ladder.md) is green.
