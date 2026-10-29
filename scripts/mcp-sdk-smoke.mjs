// SPDX-License-Identifier: Apache-2.0
// Connect-and-search through the official MCP TypeScript SDK client, the same
// StdioClientTransport Claude Desktop and Cursor use. Run it from a temp folder
// so nothing is installed system-wide:
//
//   d=$(mktemp -d) && cp scripts/mcp-sdk-smoke.mjs "$d" && cd "$d" &&
//   npm init -y >/dev/null && npm install --silent @modelcontextprotocol/sdk &&
//   node mcp-sdk-smoke.mjs /abs/path/to/target/debug/albumen-mcp
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";

const bin = process.argv[2];
const tmp = mkdtempSync(join(tmpdir(), "albumen-sdk-"));
const lib = join(tmp, "library");
for (const rel of ["2021/06/IMG_0001.jpg", "2022/01/beach_sunset.jpg"]) {
  mkdirSync(join(lib, rel, ".."), { recursive: true });
  writeFileSync(join(lib, rel), Buffer.from([0xff, 0xd8, 0xff, 0xd9])); // synthetic
}
const digest = (d) => readdirSync(d).sort().map((n) => {
  const p = join(d, n);
  return statSync(p).isDirectory() ? n + "/" + digest(p)
    : n + ":" + createHash("sha256").update(readFileSync(p)).digest("hex");
}).join(",");
const before = digest(lib);
const t0 = Date.now();
const client = new Client({ name: "albumen-sdk-smoke", version: "0" });
await client.connect(new StdioClientTransport({
  command: bin,
  args: ["--library", lib, "--index-dir", join(tmp, "index"), "--thumbnail-dir", join(tmp, "thumbs")],
}));
const { tools } = await client.listTools();
const names = tools.map((t) => t.name);
if (!names.includes("search")) throw new Error("no search tool: " + names);
const res = await client.callTool({ name: "search", arguments: { query: "beach" } });
const text = JSON.stringify(res);
if (res.isError || !text.includes("beach_sunset.jpg") || text.includes("IMG_0001")) throw new Error(text);
await client.close();
if (digest(lib) !== before) throw new Error("library changed");
console.log(`ok (sdk): ${JSON.stringify(client.getServerVersion())}; tools ${names}; search 'beach' -> 1 hit; library unchanged; ${Date.now() - t0}ms`);
