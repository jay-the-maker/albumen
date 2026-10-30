#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Assert albumen-mcp's peak RSS at 380k items against the split budget.

Linux only (reads VmHWM from /proc). Uses scripts/library-scale.py's
generator and stdio client, so the library is the same synthetic one the
library-scale job times; nothing is committed and no real photo is read.

Two servers, each: initialize, 2 priming keyword searches that match
nothing (every item visited), then 10 warm passes, with
ALBUMEN_METADATA_CACHE_ITEMS=0. VmHWM is read before the server exits.

- cold build: no search tree on disk yet; the server builds it and saves it
  on exit. Budget: peak <= 420 MiB.
- tree loaded: a second server reads that saved tree. Budget: peak
  <= 332.2 MiB.

Exit 1 when either peak is over budget, 2 when the harness itself fails.
"""

import argparse
import glob
import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("library_scale", os.path.join(HERE, "library-scale.py"))
ls = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ls)

TREE_LOADED_MIB = 332.2
COLD_BUILD_MIB = 420.0
QUERY = {"keyword": "no-such-keyword-library-scale", "limit": 100}


def vm_hwm_mib(pid):
    with open(f"/proc/{pid}/status") as handle:
        for line in handle:
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) / 1024.0
    raise SystemExit(2)


def probe(binary, library, index_dir, thumbnail_dir, warm_runs):
    env = {"ALBUMEN_SEARCH_STATS": "1", "ALBUMEN_METADATA_CACHE_ITEMS": "0", "ALBUMEN_EXIFTOOL_BATCH": "4096"}
    server = ls.Server(binary, library, index_dir, thumbnail_dir, env)
    server.send("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "rss-budget", "version": "1"}})
    for _ in range(2):
        server.search(QUERY)
    for _ in range(warm_runs):
        result = server.search(QUERY)
        if result.get("stats", {}).get("hashed") != 0:
            raise SystemExit(f"warm pass re-hashed files: {result.get('stats')}")
    peak = vm_hwm_mib(server.proc.pid)
    server.close()
    return peak


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", required=True, help="path to albumen-mcp")
    parser.add_argument("--library", required=True, help="empty directory to generate into, or a generated library with --reuse")
    parser.add_argument("--reuse", action="store_true")
    parser.add_argument("--items", type=int, default=380000)
    parser.add_argument("--index-dir", required=True, help="must be empty: the cold-build server must find no tree")
    parser.add_argument("--thumbnail-dir", required=True)
    parser.add_argument("--warm-runs", type=int, default=10)
    parser.add_argument("--json", help="write the report here")
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        raise SystemExit("rss-budget reads /proc; Linux only")
    library = os.path.abspath(args.library)
    if not args.reuse:
        with open(ls.DEFAULT_SAMPLE_JPEG, "rb") as handle:
            ls.generate(library, args.items, handle.read())
    if ls.count_files(library) != args.items:
        raise SystemExit(f"{library} does not hold {args.items} items")
    for d in (args.index_dir, args.thumbnail_dir):
        os.makedirs(d, exist_ok=True)
        if os.listdir(d):
            raise SystemExit(f"{d} must start empty")

    import subprocess

    subprocess.run([args.binary, "--library", library, "--index-dir", args.index_dir, "--build-index"], check=True, stdout=subprocess.DEVNULL)
    if glob.glob(os.path.join(args.index_dir, "**", "*.search"), recursive=True):
        raise SystemExit("a search tree exists before the cold-build server ran")

    cold = probe(args.binary, library, args.index_dir, args.thumbnail_dir, args.warm_runs)
    if not glob.glob(os.path.join(args.index_dir, "**", "*.search"), recursive=True):
        raise SystemExit("the cold-build server saved no search tree")
    loaded = probe(args.binary, library, args.index_dir, args.thumbnail_dir, args.warm_runs)

    report = {
        "items": args.items,
        "cold_build_peak_mib": round(cold, 1),
        "cold_build_budget_mib": COLD_BUILD_MIB,
        "tree_loaded_peak_mib": round(loaded, 1),
        "tree_loaded_budget_mib": TREE_LOADED_MIB,
    }
    over = []
    if cold > COLD_BUILD_MIB:
        over.append(f"cold-build peak {cold:.1f} MiB > {COLD_BUILD_MIB} MiB")
    if loaded > TREE_LOADED_MIB:
        over.append(f"tree-loaded peak {loaded:.1f} MiB > {TREE_LOADED_MIB} MiB")
    report["pass"] = not over
    print(json.dumps(report))
    if args.json:
        with open(args.json, "w") as handle:
            json.dump(report, handle, indent=2)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as handle:
            handle.write(f"### Peak RSS at {args.items:,} items\n\n| run | peak | budget |\n|---|---|---|\n"
                         f"| cold build | {cold:.1f} MiB | {COLD_BUILD_MIB} MiB |\n| tree loaded | {loaded:.1f} MiB | {TREE_LOADED_MIB} MiB |\n\n")
    if over:
        print("rss-budget: " + "; ".join(over), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
