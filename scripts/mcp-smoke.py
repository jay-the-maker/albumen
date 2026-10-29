#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Connect-and-search smoke test for albumen-mcp, the way a desktop client does it.

Spawns the server over stdio, speaks newline-delimited JSON-RPC (the MCP stdio
transport), runs initialize -> notifications/initialized -> tools/list ->
tools/call search, and checks the answer. The library is a temp folder of
synthetic files, and its bytes are compared before and after.

Standard library only. Nothing is installed.

    cargo build --locked -p albumen-mcp
    python3 scripts/mcp-smoke.py target/debug/albumen-mcp
"""
import hashlib, json, os, subprocess, sys, tempfile, threading, queue, time

def tree_digest(root):
    out = {}
    for d, _, files in os.walk(root):
        for f in files:
            p = os.path.join(d, f)
            out[os.path.relpath(p, root)] = hashlib.sha256(open(p, "rb").read()).hexdigest()
    return out

def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/debug/albumen-mcp"
    with tempfile.TemporaryDirectory() as tmp:
        lib = os.path.join(tmp, "library")
        for rel in ["2021/06/IMG_0001.jpg", "2021/06/IMG_0002.jpg", "2022/01/beach_sunset.jpg"]:
            os.makedirs(os.path.dirname(os.path.join(lib, rel)), exist_ok=True)
            with open(os.path.join(lib, rel), "wb") as f:
                f.write(b"\xff\xd8\xff\xd9")  # synthetic, not a real photo
        fixture = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "fixtures", "jpeg-gps", "IMG_0001.jpg")
        with open(fixture, "rb") as src, open(os.path.join(lib, "2022/01/fixture_gps.jpg"), "wb") as dst:
            dst.write(src.read())  # synthetic fixture from fixtures/
        before = tree_digest(lib)
        start = time.monotonic()
        proc = subprocess.Popen(
            [binary, "--library", lib,
             "--index-dir", os.path.join(tmp, "index"),
             "--thumbnail-dir", os.path.join(tmp, "thumbs")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr)
        lines = queue.Queue()
        threading.Thread(target=lambda: [lines.put(l) for l in proc.stdout], daemon=True).start()

        def send(msg):
            proc.stdin.write((json.dumps(msg) + "\n").encode()); proc.stdin.flush()

        def reply(want_id):
            while True:
                line = lines.get(timeout=15)
                if line.startswith(b"Content-Length"):
                    sys.exit("FAIL: server framed with Content-Length, not NDJSON")
                msg = json.loads(line)
                if msg.get("id") == want_id:
                    return msg

        send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "albumen-smoke", "version": "0"}}})
        init = reply(0)["result"]
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        tools = [t["name"] for t in reply(1)["result"]["tools"]]
        assert "search" in tools, tools
        assert "export" not in tools and "upload" not in tools, f"write tools offered by default: {tools}"
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": {"name": "search", "arguments": {"query": "beach"}}})
        result = reply(2)["result"]
        assert not result.get("isError"), result
        text = json.dumps(result)
        assert "beach_sunset.jpg" in text and "IMG_0001" not in text, text
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
              "params": {"name": "thumbnail", "arguments": {"path": "2022/01/fixture_gps.jpg", "size": 64}}})
        thumb = reply(3)["result"]
        assert not thumb.get("isError"), thumb
        block = thumb["content"][0]
        assert block["type"] == "image" and block["mimeType"] == "image/jpeg", block.get("type")
        import base64
        assert base64.b64decode(block["data"])[:2] == b"\xff\xd8", "not a JPEG"
        assert "data_base64" not in json.dumps(thumb["content"][1:]), "bytes also inside text"
        proc.stdin.close()
        proc.wait(timeout=10)
        assert tree_digest(lib) == before, "library changed"
        print(f"ok: {init['serverInfo']['name']} {init['serverInfo']['version']} "
              f"protocol {init.get('protocolVersion')}; tools {tools}; "
              f"search 'beach' -> 1 hit; thumbnail -> image block image/jpeg; library unchanged; {time.monotonic()-start:.2f}s")

if __name__ == "__main__":
    main()
