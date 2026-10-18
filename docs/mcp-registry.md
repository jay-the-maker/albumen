# MCP registry draft

This is metadata for listing `albumen-mcp`. It is a draft. Do not submit it to the official MCP Registry or to any other list.

The crate is `albumen-mcp` 0.1.0 in `mcp/`. The workspace sets `publish = false`. There is no crates.io release, no container image, and no MCPB artifact. A registry that requires a published package will reject this entry until one exists. Publishing the crate, and flipping `publish`, is a separate decision.

## Official MCP Registry

Schema in use: `https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json`.

The registry name would be `io.github.jay-the-maker/albumen`. The `description` field is limited to 100 characters. Cargo ownership checks read the rendered README on crates.io and require the `mcp-name:` token as visible markdown. An HTML comment is stripped and does not count. That token is not in the README today, because the crate is not published.

`albumen-mcp` speaks stdio only. There is no remote URL. Clients install with `cargo install albumen-mcp` once a release exists, then run the binary by name. Arguments:

```text
albumen-mcp --library <directory>
albumen-mcp --library <directory> --enable-export --enable-upload
```

Tools that exist in `mcp/src/tools.rs`:

| Tool | When it is offered | What it does |
| --- | --- | --- |
| `search` | always | Read files and XMP/AAE sidecars. Filter on path, capture date, camera, lens, GPS box, keyword, caption. |
| `browse` | always | List files under an optional relative prefix. Reads only. |
| `export` | `--enable-export` | `copy_verify` the library to a directory or `s3://` prefix. Does not overwrite. Does not modify the library. |
| `upload` | `--enable-upload` | `copy_verify` a file and its discovered sidecars and pairs into the library. Does not modify or remove the source. |

`delete`, `remove`, `unlink`, `move`, `move_verified`, `quarantine`, and `copy_verify_quarantine` are refused.

Draft `server.json`, not submitted:

```json
{
  "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
  "name": "io.github.jay-the-maker/albumen",
  "title": "Albumen",
  "description": "Bit-exact photo library over stdio. Search and browse; export and upload are opt-in.",
  "websiteUrl": "https://github.com/jay-the-maker/albumen",
  "repository": {
    "url": "https://github.com/jay-the-maker/albumen",
    "source": "github",
    "subfolder": "mcp"
  },
  "version": "0.1.0",
  "packages": [
    {
      "registryType": "cargo",
      "registryBaseUrl": "https://crates.io",
      "identifier": "albumen-mcp",
      "version": "0.1.0",
      "transport": {
        "type": "stdio"
      },
      "packageArguments": [
        {
          "type": "named",
          "name": "--library",
          "description": "Directory of originals. Search and browse only read it.",
          "isRequired": true,
          "format": "filepath"
        },
        {
          "type": "named",
          "name": "--enable-export",
          "description": "Offer the export tool. Off unless set. Writes through copy_verify and does not overwrite.",
          "isRequired": false
        },
        {
          "type": "named",
          "name": "--enable-upload",
          "description": "Offer the upload tool. Off unless set. Writes through copy_verify and does not remove the source.",
          "isRequired": false
        }
      ]
    }
  ]
}
```

Before any submission, the README that crates.io will render needs a visible line:

```markdown
- MCP Registry name: `mcp-name: io.github.jay-the-maker/albumen`
```

Publish with the official `mcp-publisher` tool only after `albumen-mcp` 0.1.0 is on crates.io and that token matches. Do not invent a package version that is not in `mcp/Cargo.toml`.

## Other lists

These are the same facts, shaped for lists that are not the official registry. None of them have been submitted.

### modelcontextprotocol/servers

The community list defers to the official registry. A README entry, if one is ever added by hand, would be:

```markdown
- [Albumen](https://github.com/jay-the-maker/albumen) — Bit-exact, bring-your-own-storage photo library. `albumen-mcp` over stdio: `search` and `browse` always; `export` and `upload` only with flags. No delete tool.
```

### Glama, Smithery, PulseMCP, mcp.so

Each of these wants a name, a one-line description, the repository URL, stdio as the transport, and an install command. Use the same description as `server.json`. Install, once the crate is public:

```text
cargo install albumen-mcp --version 0.1.0
albumen-mcp --library /path/to/library
```

Do not list a remote endpoint. Do not mark export or upload as default tools. Do not offer a delete tool.
