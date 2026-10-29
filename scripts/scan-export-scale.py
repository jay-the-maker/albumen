#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Time `albumen scan` and `albumen export` on a generated synthetic library.

Every file is generated here: a few hundred bytes of text with a unique
counter, spread over nested directories. No photo or media file is read or
committed. The library, the manifest and the export destination all live
under --workdir (ALBUMEN_HOME too), which must not exist yet.

--check compares the run with the "scan_export" entry in bench/baseline.json
and fails when scan or export is more than 20 percent slower per 1,000
files. While that entry has "placeholder": true, it prints
"BASELINE PLACEHOLDER: regression check not enforced" (as a ::warning::
annotation on GitHub Actions) and does not compare. --write prints the entry to put in the baseline. --keep leaves the
generated tree in place; otherwise only the paths this run created are
removed.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time

TOLERANCE = 0.20
ENV = dict(os.environ)
PER_DIR = 500


def generate(library, files):
    for i in range(files):
        d = os.path.join(library, f"{i // (PER_DIR * 100):03d}", f"{(i // PER_DIR) % 100:02d}")
        if i % PER_DIR == 0:
            os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, f"synthetic-{i:07d}.txt"), "w", encoding="ascii") as f:
            f.write(f"albumen synthetic scale file {i}\n" + ("x" * (i % 256)) + "\n")


def timed(cmd):
    start = time.monotonic()
    proc = subprocess.run(cmd, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    seconds = time.monotonic() - start
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout + proc.stderr)
        raise SystemExit(f"{cmd[1]} exited {proc.returncode}")
    return seconds


def count_lines(path):
    with open(path, encoding="utf-8") as f:
        return sum(1 for line in f if line.strip())


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--albumen", required=True)
    p.add_argument("--files", type=int, default=100_000)
    p.add_argument("--workdir", required=True)
    p.add_argument("--baseline", default="bench/baseline.json")
    mode = p.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    p.add_argument("--keep", action="store_true")
    a = p.parse_args()
    if a.files < 1:
        raise SystemExit("--files must be at least 1")
    if os.path.exists(a.workdir):
        raise SystemExit(f"{a.workdir} already exists; pass a new path")

    library = os.path.join(a.workdir, "source", "library")
    manifest = os.path.join(a.workdir, "scan.jsonl")
    ledger = os.path.join(a.workdir, "ledger")
    dest = os.path.join(a.workdir, "export")
    os.makedirs(library)
    ENV["ALBUMEN_HOME"] = os.path.join(a.workdir, "home")
    try:
        start = time.monotonic()
        generate(library, a.files)
        gen = time.monotonic() - start
        scan = timed([a.albumen, "scan", library, "--manifest", manifest, "--ledger", ledger])
        lines = count_lines(manifest)
        if lines < a.files:
            raise SystemExit(f"scan wrote {lines} lines for {a.files} files")
        export = timed([a.albumen, "export", "--from", library, "--to", dest, "--apply"])
        exported = sum(len(fs) for _, _, fs in os.walk(dest))
        if exported < a.files:
            raise SystemExit(f"export wrote {exported} files for {a.files} files")
        per_k = 1000.0 / a.files
        run = {
            "files": a.files,
            "scan_s_per_1k": round(scan * per_k, 4),
            "export_s_per_1k": round(export * per_k, 4),
        }
        print(json.dumps({"generate_s": round(gen, 2), "scan_s": round(scan, 2),
                          "export_s": round(export, 2), **run}, indent=2))
        if a.write:
            print(json.dumps({"scan_export": run}, indent=2))
        if a.check:
            with open(a.baseline, encoding="utf-8") as f:
                base = json.load(f).get("scan_export")
            if base is None:
                raise SystemExit(f"{a.baseline} has no scan_export entry")
            if base.get("placeholder"):
                line = "BASELINE PLACEHOLDER: regression check not enforced"
                print(line)
                if os.environ.get("GITHUB_ACTIONS") == "true":
                    print(f"::warning::{line}")
                else:
                    print(line, file=sys.stderr)
                return
            problems = []
            for key in ("scan_s_per_1k", "export_s_per_1k"):
                limit = base[key] * (1 + TOLERANCE)
                if run[key] > limit:
                    problems.append(f"{key} {run[key]} is more than 20 percent over {base[key]}")
            if problems:
                raise SystemExit("\n".join(problems))
            print(f"within 20 percent of {a.baseline}", file=sys.stderr)
    finally:
        if a.keep:
            print(f"kept {a.workdir}", file=sys.stderr)
        else:
            for path in (os.path.join(a.workdir, "source"), manifest, ledger, dest, ENV["ALBUMEN_HOME"]):
                if os.path.isdir(path):
                    shutil.rmtree(path)
                elif os.path.exists(path):
                    os.remove(path)
            os.rmdir(a.workdir)


if __name__ == "__main__":
    main()
