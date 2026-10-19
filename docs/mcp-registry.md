# MCP registry

`server.json` at the repository root is the official MCP Registry metadata for `io.github.jay-the-maker/albumen`. It is checked against `https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json`. The description is the tagline and is under the schema's 100-character limit. It does not claim a catalog, a hosted library, or a delete tool.

The package is the OCI image `ghcr.io/jay-the-maker/albumen-mcp:0.1.1`, transport stdio. The image entrypoint is `albumen-mcp --library /library`. Search and browse read that directory. Export and upload are off unless the client appends `--enable-export` or `--enable-upload`. Those tools still return a plan and write nothing unless the process was also started with `--apply` or `--execute`.

The default registry mount is `type=bind,src={library_path},dst=/library,readonly`. That mount is the library of originals. It refuses create, modify, rename, and delete. Opt-in export and upload do not make it writable. When a writable directory is needed, it is a separate optional mount: `type=bind,src={destination_path},dst=/destination`. `/destination` is not the library of originals. Export writes there when `to` is `/destination` (or an `s3://` prefix). Upload copies into the library path the process was given; this mount keeps that path read-only, so the originals stay as they are.

`.github/workflows/publish-mcp.yml` is the publish path. It runs on a `v*` tag, pushes that image with `GITHUB_TOKEN`, then runs `mcp-publisher login github-oidc` and `mcp-publisher publish`. The tag, `server.json`, and `mcp/Cargo.toml` must be the same version. Do not publish by hand.

`glama.json` records the Glama maintainer. It is not a submission. Smithery, PulseMCP, mcp.so, and the `modelcontextprotocol/servers` list are not submitted from this repository. The MCP Bundle on the GitHub release is documented in [mcpb.md](mcpb.md). The release workflow does not publish that bundle to Smithery.

## Tools

These are the tools in `mcp/src/tools.rs`:

| Tool | When it is offered | What it does |
| --- | --- | --- |
| `search` | always | Read files and XMP/AAE sidecars. Filter on path, capture date, camera, lens, GPS box, keyword, caption. |
| `browse` | always | List files under an optional relative prefix. Reads only. |
| `export` | `--enable-export` | `copy_verify` the library to a directory or `s3://` prefix when the process was started with `--apply` or `--execute`. Otherwise it returns a plan. Does not overwrite. Does not modify the library. A destination inside the library is refused. |
| `upload` | `--enable-upload` | `copy_verify` a file and its discovered sidecars and pairs into the library when the process was started with `--apply` or `--execute`. Otherwise it returns a plan. Does not modify or remove the source. |

`delete`, `remove`, `unlink`, `move`, `move_verified`, `quarantine`, and `copy_verify_quarantine` are refused.

The crate stays `publish = false`. There is no crates.io release. The registry package is the container, not Cargo.
