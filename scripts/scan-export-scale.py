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
import threading
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


def run_measured(cmd, env):
    """Run cmd; return (seconds, peak RSS in KiB) from wait4 on this child."""
    start = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    out, err = [], []

    def drain(pipe, sink):
        for line in iter(pipe.readline, ""):
            sink.append(line)
        pipe.close()

    readers = [threading.Thread(target=drain, args=(proc.stdout, out)),
               threading.Thread(target=drain, args=(proc.stderr, err))]
    for reader in readers:
        reader.start()
    for reader in readers:
        reader.join()
    _, status, usage = os.wait4(proc.pid, 0)
    proc.returncode = os.waitstatus_to_exitcode(status)
    seconds = time.monotonic() - start
    peak_kib = usage.ru_maxrss // 1024 if sys.platform == "darwin" else usage.ru_maxrss
    if proc.returncode != 0:
        sys.stderr.write("".join(out) + "".join(err))
        raise SystemExit(f"{cmd[1]} exited {proc.returncode}")
    return seconds, peak_kib


def timed(cmd):
    return run_measured(cmd, ENV)


def legs_for(event, files_input):
    """File counts for one workflow event. The 1,000,000 leg is cron only."""
    if event == "pull_request":
        legs = [100_000]
    elif event == "schedule":
        legs = [100_000, 380_000, 1_000_000]
    elif event == "workflow_dispatch":
        legs = []
        for part in (files_input or "").split(","):
            part = part.strip()
            if not part:
                continue
            try:
                value = int(part)
            except ValueError:
                raise SystemExit(f"invalid leg value {part}") from None
            if value < 1:
                raise SystemExit(f"invalid leg value {part}")
            legs.append(value)
        legs = legs or [100_000]
    else:
        raise SystemExit(f"unknown event {event}")
    return legs


def expect_exit(fn, text):
    try:
        fn()
    except SystemExit as err:
        assert text in str(err), err
    else:
        raise AssertionError(f"expected SystemExit containing {text!r}")


def self_test():
    assert legs_for("pull_request", "") == [100_000]
    assert legs_for("schedule", "") == [100_000, 380_000, 1_000_000]
    assert legs_for("workflow_dispatch", "200000, 300000") == [200_000, 300_000]
    assert legs_for("workflow_dispatch", "") == [100_000]
    assert 1_000_000 not in legs_for("pull_request", "")
    expect_exit(lambda: legs_for("workflow_dispatch", "0,100000"), "invalid leg value")
    expect_exit(lambda: legs_for("workflow_dispatch", "ten"), "invalid leg value")
    expect_exit(lambda: legs_for("push", ""), "unknown event")
    seconds, rss = run_measured([sys.executable, "-c", "x = bytearray(50 * 1024 * 1024); print(len(x))"], dict(os.environ))
    assert rss >= 50_000 and seconds > 0, (rss, seconds)
    expect_exit(lambda: run_measured([sys.executable, "-c", "raise SystemExit(3)"], dict(os.environ)), "exited 3")
    assert baseline_problems({"placeholder": True}, {"scan_s_per_1k": 99.0, "export_s_per_1k": 99.0}) is None
    base = {"scan_s_per_1k": 1.0, "export_s_per_1k": 1.0}
    assert baseline_problems(base, {"scan_s_per_1k": 1.2, "export_s_per_1k": 1.0}) == []
    assert len(baseline_problems(base, {"scan_s_per_1k": 1.3, "export_s_per_1k": 1.0})) == 1
    print("self-test ok")


def baseline_problems(base, run):
    """None while the baseline is a placeholder (warn only), else the regressions."""
    if base.get("placeholder"):
        return None
    problems = []
    for key in ("scan_s_per_1k", "export_s_per_1k"):
        limit = base[key] * (1 + TOLERANCE)
        if run[key] > limit:
            problems.append(f"{key} {run[key]} is more than 20 percent over {base[key]}")
    return problems


def count_lines(path):
    with open(path, encoding="utf-8") as f:
        return sum(1 for line in f if line.strip())


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--albumen")
    p.add_argument("--files", type=int, default=100_000)
    p.add_argument("--workdir")
    p.add_argument("--baseline", default="bench/baseline.json")
    mode = p.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    p.add_argument("--keep", action="store_true")
    p.add_argument("--json-out", help="also write the run JSON here (the CI artifact)")
    p.add_argument("--self-test", action="store_true")
    p.add_argument("--legs", metavar="EVENT", help="print the JSON list of file counts for a workflow event")
    p.add_argument("--legs-input", default="")
    if "--self-test" in sys.argv[1:]:
        self_test()
        return
    if "--legs" in sys.argv[1:]:
        a, _ = p.parse_known_args()
        print(json.dumps(legs_for(a.legs, a.legs_input)))
        return
    a = p.parse_args()
    if not a.albumen:
        p.error("--albumen is required")
    if not a.workdir:
        p.error("--workdir is required")
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
        scan, scan_rss = timed([a.albumen, "scan", library, "--manifest", manifest, "--ledger", ledger])
        lines = count_lines(manifest)
        if lines < a.files:
            raise SystemExit(f"scan wrote {lines} lines for {a.files} files")
        export, export_rss = timed([a.albumen, "export", "--from", library, "--to", dest, "--apply"])
        exported = sum(len(fs) for _, _, fs in os.walk(dest))
        if exported < a.files:
            raise SystemExit(f"export wrote {exported} files for {a.files} files")
        per_k = 1000.0 / a.files
        run = {
            "files": a.files,
            "scan_s_per_1k": round(scan * per_k, 4),
            "export_s_per_1k": round(export * per_k, 4),
        }
        result = {"generate_s": round(gen, 2), "scan_s": round(scan, 2),
                  "export_s": round(export, 2), "scan_peak_rss_kib": scan_rss,
                  "export_peak_rss_kib": export_rss, **run}
        print(json.dumps(result, indent=2))
        if a.json_out:
            with open(a.json_out, "w", encoding="utf-8") as f:
                json.dump(result, f, indent=2)
                f.write("\n")
        if a.write:
            print(json.dumps({"scan_export": run}, indent=2))
        if a.check:
            with open(a.baseline, encoding="utf-8") as f:
                base = json.load(f).get("scan_export")
            if base is None:
                raise SystemExit(f"{a.baseline} has no scan_export entry")
            problems = baseline_problems(base, run)
            if problems is None:
                line = "BASELINE PLACEHOLDER: regression check not enforced"
                print(line)
                if os.environ.get("GITHUB_ACTIONS") == "true":
                    print(f"::warning::{line}")
                else:
                    print(line, file=sys.stderr)
                return
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
