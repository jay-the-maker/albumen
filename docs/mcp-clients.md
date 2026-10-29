# Using albumen-mcp from Claude Desktop and Cursor

`albumen-mcp` is a stdio MCP server. It speaks newline-delimited JSON-RPC, the
MCP stdio transport. By default it is read-only and offers `search`, `browse`,
`ledger` and `thumbnail`. `export` and `upload` appear only with
`--enable-export` / `--enable-upload`, and they write only with `--apply`.

Use absolute paths. Desktop clients do not start in your shell's directory and
may not inherit your `PATH`.

## Claude Desktop

Edit `claude_desktop_config.json`:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "albumen": {
      "command": "/usr/local/bin/albumen-mcp",
      "args": ["--library", "/Volumes/Photos/Library"]
    }
  }
}
```

Quit and reopen Claude Desktop. The server shows up under the tools menu.

## Cursor

Put this in `~/.cursor/mcp.json` (all projects) or `.cursor/mcp.json` (one project):

```json
{
  "mcpServers": {
    "albumen": {
      "command": "/usr/local/bin/albumen-mcp",
      "args": ["--library", "/Volumes/Photos/Library"]
    }
  }
}
```

On Windows, use `"command": "C:\\Program Files\\albumen\\albumen-mcp.exe"` and
a library path like `"D:\\Photos\\Library"`.

## Smoke test

These steps build the server, then connect and search over stdio against a
temp library of synthetic files (plus `fixtures/jpeg-gps/IMG_0001.jpg` for `thumbnail`). They also check that the library bytes are
unchanged.

```sh
cargo build --locked -p albumen-mcp
python3 scripts/mcp-smoke.py target/debug/albumen-mcp        # stdlib only
```

The next steps use the official MCP TypeScript SDK client. It is installed
into a temp folder, so nothing goes in system-wide:

```sh
d=$(mktemp -d) && cp scripts/mcp-sdk-smoke.mjs "$d" && cd "$d" &&
npm init -y >/dev/null && npm install --silent @modelcontextprotocol/sdk &&
node mcp-sdk-smoke.mjs /abs/path/to/target/debug/albumen-mcp /abs/path/to/fixtures/jpeg-gps/IMG_0001.jpg
```

## Known limits

- **Clients tested.** Both smoke scripts run on Linux. The Python script uses
  raw NDJSON. The Node script uses `@modelcontextprotocol/sdk` 1.32.2 with
  `StdioClientTransport`. Claude Desktop and Cursor themselves have **not**
  been driven end to end. The snippets above follow each client's documented
  config format, and both clients use that same SDK stdio transport.
- **Transport.** Only stdio is supported. There is no HTTP, SSE or remote
  transport, so a hosted or remote client cannot connect.
- **Protocol.** The server answers `initialize` with protocol `2025-06-18`. It
  offers tools only: no resources, prompts, sampling or progress notifications.
- **First search.** The first search on a large library builds the index. Run
  `albumen-mcp --library ... --build-index` once beforehand so the first
  client call does not wait or time out.
- **One library per entry.** Add a second `mcpServers` entry for another library.
- **Writes.** `export` and `upload` need explicit flags. There is no delete tool.
- **Thumbnails.** `thumbnail` returns an MCP image content block,
  `{"type": "image", "data": <base64>, "mimeType": "image/jpeg"}`, followed by a
  text block with path, width, height and cache state. Both smoke scripts check
  the block type, and the SDK client accepts it. Whether Claude Desktop or Cursor
  draws the picture inline has not been checked in those apps.
