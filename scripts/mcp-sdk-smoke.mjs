// SPDX-License-Identifier: Apache-2.0
// Connect-and-search through the official MCP TypeScript SDK client, the same
// StdioClientTransport Claude Desktop and Cursor use. Run it from a temp folder
// so nothing is installed system-wide:
//
//   d=$(mktemp -d) && cp scripts/mcp-sdk-smoke.mjs "$d" && cd "$d" &&
//   npm init -y >/dev/null && npm install --silent @modelcontextprotocol/sdk &&
//   node mcp-sdk-smoke.mjs /abs/path/to/target/debug/albumen-mcp /abs/path/to/fixtures/jpeg-gps/IMG_0001.jpg
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";

const bin = process.argv[2];
const fixture = process.argv[3]; // synthetic fixtures/jpeg-gps/IMG_0001.jpg
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
writeFileSync(join(lib, "2022/01/fixture_gps.jpg"), readFileSync(fixture));
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
const thumb = await client.callTool({ name: "thumbnail", arguments: { path: "2022/01/fixture_gps.jpg", size: 64 } });
const block = thumb.content[0];
if (thumb.isError || block.type !== "image" || block.mimeType !== "image/jpeg") throw new Error(JSON.stringify(thumb).slice(0, 300));
if (Buffer.from(block.data, "base64").subarray(0, 2).toString("hex") !== "ffd8") throw new Error("not a JPEG");
if (JSON.stringify(thumb.content.slice(1)).includes("data_base64")) throw new Error("bytes also inside text");
await client.close();
if (digest(lib) !== before) throw new Error("library changed");
console.log(`ok (sdk): ${JSON.stringify(client.getServerVersion())}; tools ${names}; search 'beach' -> 1 hit; thumbnail -> image block image/jpeg; library unchanged; ${Date.now() - t0}ms`);
