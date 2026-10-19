#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build a synthetic library and time albumen-mcp against it.

The library is generated at test time and never committed. Every item is a
4-byte synthetic JPEG (FF D8 FF D9) in a YYYY/MM/ tree with camera-style
names, so the walk sees the same directory shape a photo library has. No
real photo is read. The generator is deterministic: the same --items and
--seed give the same paths, and the script prints a SHA-256 of the sorted
path list so two runs can be compared.

Measured through the real stdio MCP server (`albumen-mcp --library`):

- first index build: `albumen-mcp --build-index` on a library with no saved
  index, timed on its own. The index is saved outside the library, under
  --index-dir. This is a one-time cost and has no target.
- cold start: process spawn until `initialize` and `tools/list` have both
  answered, which is the moment the first query can be sent. The first
  search's own latency is reported beside it.
- search: at least 200 fixed `search` calls, nearest-rank p50 and p95.
- peak memory: peak RSS (Linux, macOS) or peak working set (Windows) of the
  albumen-mcp process.

Targets: cold start <= 1.5 s, time to first answer <= 1.5 s, search p95
<= 200 ms. This is report-only:
the exit code is non-zero only when the harness itself fails, not when a
number misses a target.
"""

import argparse
import hashlib
import json
import os
import random
import subprocess
import sys
import time

COLD_START_TARGET_MS = 1500.0
FIRST_ANSWER_TARGET_MS = 1500.0
SEARCH_P95_TARGET_MS = 200.0
JPEG = b"\xff\xd8\xff\xd9"
PREFIXES = ("IMG_", "DSC_", "PXL_", "DJI_")


def item_path(index, items):
    # Spread items evenly over 2010-01 .. 2026-12 (204 months).
    month_index = index * 204 // items
    year = 2010 + month_index // 12
    month = 1 + month_index % 12
    prefix = PREFIXES[index % len(PREFIXES)]
    return f"{year:04d}/{month:02d}/{prefix}{index:07d}.jpg"


def generate(root, items):
    os.makedirs(root, exist_ok=True)
    if os.listdir(root):
        raise SystemExit(f"refusing to generate into a non-empty directory: {root}")
    digest = hashlib.sha256()
    made = set()
    for index in range(items):
        relative = item_path(index, items)
        parent = os.path.dirname(relative)
        if parent not in made:
            os.makedirs(os.path.join(root, parent), exist_ok=True)
            made.add(parent)
        with open(os.path.join(root, relative), "wb") as handle:
            handle.write(JPEG)
        digest.update(relative.encode() + b"\n")
    return digest.hexdigest()


def count_files(root):
    total = 0
    for _, _, files in os.walk(root):
        total += len(files)
    return total


def fixed_queries(items, count, seed):
    """Deterministic mix of hits, page-filling prefixes, and misses."""
    rng = random.Random(seed)
    queries = []
    for n in range(count):
        kind = n % 4
        if kind in (0, 1):
            index = rng.randrange(items)
            name = os.path.basename(item_path(index, items))[:-4].lower()
            queries.append({"query": name, "limit": 100})
        elif kind == 2:
            index = rng.randrange(items)
            month = os.path.dirname(item_path(index, items)) + "/"
            queries.append({"query": month, "limit": 100})
        else:
            queries.append({"query": f"no-such-item-{rng.randrange(10**9):09d}", "limit": 100})
    return queries


class Server:
    def __init__(self, binary, library, index_dir):
        self.started = time.perf_counter()
        self.proc = subprocess.Popen(
            [binary, "--library", library, "--index-dir", index_dir],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr,
            bufsize=0,
        )
        self.next_id = 0

    def send(self, method, params):
        self.next_id += 1
        body = json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}).encode()
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.proc.stdin.flush()
        return self.read(self.next_id)

    def read(self, want):
        out = self.proc.stdout
        while True:
            length = None
            while True:
                line = out.readline()
                if not line:
                    raise SystemExit("albumen-mcp closed stdout")
                if line in (b"\r\n", b"\n"):
                    break
                name, _, value = line.decode().partition(":")
                if name.strip().lower() == "content-length":
                    length = int(value.strip())
            body = b""
            while len(body) < length:
                chunk = out.read(length - len(body))
                if not chunk:
                    raise SystemExit("albumen-mcp closed stdout mid-body")
                body += chunk
            message = json.loads(body)
            if message.get("id") == want:
                if "error" in message:
                    raise SystemExit(f"albumen-mcp error: {message['error']}")
                return message["result"]

    def search(self, arguments):
        result = self.send("tools/call", {"name": "search", "arguments": arguments})
        if result.get("isError"):
            raise SystemExit(f"search {arguments} failed: {result['content'][0]['text']}")
        return json.loads(result["content"][0]["text"])

    def peak_bytes(self):
        """Peak RSS / working set while the process is alive, or None."""
        if sys.platform.startswith("linux"):
            with open(f"/proc/{self.proc.pid}/status") as handle:
                for line in handle:
                    if line.startswith("VmHWM:"):
                        return int(line.split()[1]) * 1024
        if os.name == "nt":
            import ctypes
            from ctypes import wintypes

            class Counters(ctypes.Structure):
                _fields_ = [
                    ("cb", wintypes.DWORD),
                    ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t),
                    ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t),
                    ("PeakPagefileUsage", ctypes.c_size_t),
                ]

            counters = Counters()
            counters.cb = ctypes.sizeof(Counters)
            psapi = ctypes.WinDLL("psapi")
            psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
            handle = wintypes.HANDLE(int(self.proc._handle))
            if psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                return int(counters.PeakWorkingSetSize)
        return None

    def close(self):
        self.proc.stdin.close()
        code = self.proc.wait(timeout=120)
        if code != 0:
            raise SystemExit(f"albumen-mcp exited {code}")
        if sys.platform == "darwin":
            import resource

            # ru_maxrss is bytes on macOS. albumen-mcp is the only child.
            return resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        return None


def nearest_rank(sorted_values, pct):
    rank = max(1, -(-len(sorted_values) * pct // 100))
    return sorted_values[int(rank) - 1]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", required=True, help="path to albumen-mcp")
    parser.add_argument("--library", required=True, help="empty directory to generate into, or an existing generated library with --reuse")
    parser.add_argument("--items", type=int, default=380000)
    parser.add_argument("--queries", type=int, default=200)
    parser.add_argument("--seed", type=int, default=380000)
    parser.add_argument("--leg", default=sys.platform)
    parser.add_argument("--reuse", action="store_true", help="time an existing generated library")
    parser.add_argument("--json", help="write the report here")
    parser.add_argument("--index-dir", help="where albumen-mcp saves its index (default: a sibling of --library)")
    args = parser.parse_args()
    if args.queries < 200 and args.items >= 380000:
        raise SystemExit("the 380,000-item run uses at least 200 queries")

    library = os.path.abspath(args.library)
    generate_s = 0.0
    paths_sha256 = None
    if not args.reuse:
        began = time.perf_counter()
        paths_sha256 = generate(library, args.items)
        generate_s = time.perf_counter() - began
    on_disk = count_files(library)
    if on_disk != args.items:
        raise SystemExit(f"library has {on_disk} files, expected {args.items}")

    index_dir = os.path.abspath(args.index_dir or library.rstrip("/\\") + ".albumen-index")
    if os.path.commonpath([index_dir, library]) == library:
        raise SystemExit("--index-dir must be outside --library")
    os.makedirs(index_dir, exist_ok=True)
    if os.listdir(index_dir) and not args.reuse:
        raise SystemExit(f"index directory is not empty: {index_dir}")

    # The first index build, timed on its own.
    began = time.perf_counter()
    built = subprocess.run(
        [args.binary, "--library", library, "--index-dir", index_dir, "--build-index"],
        check=True,
        stdout=subprocess.PIPE,
    )
    index_build_ms = (time.perf_counter() - began) * 1000.0
    index_report = json.loads(built.stdout)
    print(f"library-scale {args.leg}: first index build {index_build_ms:.1f} ms ({index_report})", file=sys.stderr, flush=True)
    library_files_after_build = count_files(library)
    if library_files_after_build != args.items:
        raise SystemExit(f"index build changed the library file count to {library_files_after_build}")

    server = Server(args.binary, library, index_dir)
    server.send("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "library-scale", "version": "1"}})
    tools = server.send("tools/list", {})
    ready_ms = (time.perf_counter() - server.started) * 1000.0
    print(f"library-scale {args.leg}: generated {args.items} items in {generate_s:.1f} s, ready in {ready_ms:.1f} ms", file=sys.stderr, flush=True)
    if not any(tool.get("name") == "search" for tool in tools.get("tools", [])):
        raise SystemExit("albumen-mcp did not list the search tool")

    queries = fixed_queries(args.items, args.queries, args.seed)
    began = time.perf_counter()
    first = server.search(queries[0])
    first_query_ms = (time.perf_counter() - began) * 1000.0
    first_answer_ms = (time.perf_counter() - server.started) * 1000.0

    latencies = []
    hits = 0
    for arguments in queries:
        began = time.perf_counter()
        page = server.search(arguments)
        latencies.append((time.perf_counter() - began) * 1000.0)
        hits += len(page.get("files", []))
        done = len(latencies)
        if done % 20 == 0:
            so_far = sorted(latencies)
            print(f"library-scale {args.leg}: {done}/{len(queries)} queries, p50 so far {nearest_rank(so_far, 50):.1f} ms, p95 so far {nearest_rank(so_far, 95):.1f} ms", file=sys.stderr, flush=True)
        if arguments["query"].startswith("no-such-item") and page.get("files"):
            raise SystemExit(f"miss query {arguments['query']} returned files")
        if not arguments["query"].startswith("no-such-item") and not page.get("files"):
            raise SystemExit(f"hit query {arguments['query']} returned nothing")

    peak = server.peak_bytes()
    after_exit = server.close()
    if peak is None:
        peak = after_exit
    latencies.sort()
    report = {
        "leg": args.leg,
        "items": args.items,
        "queries": len(queries),
        "seed": args.seed,
        "paths_sha256": paths_sha256,
        "generate_seconds": round(generate_s, 3),
        "first_index_build_ms": round(index_build_ms, 1),
        "first_index_build_in_process_ms": index_report.get("index_ms"),
        "cold_start_ms": round(ready_ms, 1),
        "first_query_ms": round(first_query_ms, 1),
        "spawn_to_first_answer_ms": round(first_answer_ms, 1),
        "search_p50_ms": round(nearest_rank(latencies, 50), 1),
        "search_p95_ms": round(nearest_rank(latencies, 95), 1),
        "search_max_ms": round(latencies[-1], 1),
        "peak_memory_mib": round(peak / 1048576.0, 1) if peak else None,
        "hits_returned": hits,
        "cold_start_target_ms": COLD_START_TARGET_MS,
        "search_p95_target_ms": SEARCH_P95_TARGET_MS,
        "first_answer_target_ms": FIRST_ANSWER_TARGET_MS,
        "first_answer_met": first_answer_ms <= FIRST_ANSWER_TARGET_MS,
        "cold_start_met": ready_ms <= COLD_START_TARGET_MS,
        "search_p95_met": nearest_rank(latencies, 95) <= SEARCH_P95_TARGET_MS,
        "first_result_count": len(first.get("files", [])),
    }
    text = json.dumps(report, indent=2)
    print(text)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        def mark(met):
            return "met" if met else "not yet"
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(f"### library scale ({args.leg}, {args.items:,} items)\n\n")
            handle.write("| Metric | Measured | Target | Status |\n| --- | --- | --- | --- |\n")
            handle.write(f"| Cold start (spawn to first query ready) | {report['cold_start_ms']} ms | ≤ 1500 ms | {mark(report['cold_start_met'])} |\n")
            handle.write(f"| Search p95 over {len(queries)} fixed queries | {report['search_p95_ms']} ms | ≤ 200 ms | {mark(report['search_p95_met'])} |\n")
            handle.write(f"| Search p50 | {report['search_p50_ms']} ms | | |\n")
            handle.write(f"| First query after ready | {report['first_query_ms']} ms | | |\n")
            handle.write(f"| Time to first answer (spawn to first search result, saved index) | {report['spawn_to_first_answer_ms']} ms | ≤ 1500 ms | {mark(report['first_answer_met'])} |\n")
            handle.write(f"| First index build (one time, reported separately) | {report['first_index_build_ms']} ms | | |\n")
            handle.write(f"| Peak memory | {report['peak_memory_mib']} MiB | | |\n")
            handle.write(f"| Generate library (not timed against a target) | {report['generate_seconds']} s | | |\n\n")
            handle.write("Report only. A miss is a measurement, not a failed build.\n\n")


if __name__ == "__main__":
    main()
