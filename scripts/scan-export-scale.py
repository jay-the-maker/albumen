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
annotation on GitHub Actions) and does not compare, until a real baseline has
been recorded: --baseline-out sets the top-level
"scan_export_real_baseline_recorded" marker, and once the file carries it a
placeholder scan_export fails --check instead of warning. --write prints the entry to put in the baseline.
--baseline-out writes a whole baseline file with this run as scan_export, the
placeholder removed and the runner and commit SHA recorded; pull-request CI
uploads it as the "baseline" artifact. --keep leaves the
generated tree in place; otherwise only the paths this run created are
removed.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
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


def touch_one_percent(library, files):
    """Append a byte to every 100th generated file; return how many."""
    touched = 0
    for i in range(0, files, 100):
        d = os.path.join(library, f"{i // (PER_DIR * 100):03d}", f"{(i // PER_DIR) % 100:02d}")
        with open(os.path.join(d, f"synthetic-{i:07d}.txt"), "a", encoding="ascii") as f:
            f.write("t")
        touched += 1
    return touched


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
    run = {"files": 100_000, "scan_s_per_1k": 0.5, "export_s_per_1k": 0.7}
    entry = baseline_entry(run, "ubuntu-24.04", "abc123")
    assert "placeholder" not in entry and entry["runner"] == "ubuntu-24.04" and entry["commit"] == "abc123"
    assert baseline_problems(entry, run) == []
    # Follow-up: after the real recording, a placeholder fails the check.
    with tempfile.TemporaryDirectory() as td:
        out = os.path.join(td, "baseline.json")
        data = write_baseline_file(out, {"scan_export": {"placeholder": True}}, entry)
        assert data[REAL_MARKER] == {"runner": "ubuntu-24.04", "commit": "abc123"}
        with open(out, encoding="utf-8") as f:
            assert json.load(f)[REAL_MARKER]["commit"] == "abc123"
    placeholder = {"placeholder": True, "scan_s_per_1k": 1.0, "export_s_per_1k": 1.0}
    assert baseline_problems(placeholder, run, real_recorded=False) is None
    failed = baseline_problems(placeholder, run, real_recorded=True)
    assert failed and "placeholder" in failed[0]
    assert baseline_problems(entry, run, real_recorded=True) == []
    assert len(baseline_problems(entry, {"scan_s_per_1k": 0.61, "export_s_per_1k": 0.7}, True)) == 1
    expect_exit(lambda: baseline_entry(run, "", "abc123"), "runner name")
    print("self-test ok")


def baseline_entry(run, runner, commit):
    """The scan_export entry CI records: never a placeholder, runner and commit named."""
    if not runner or not commit:
        raise SystemExit("--baseline-out needs a runner name and a commit SHA")
    entry = {k: run[k] for k in ("files", "scan_s_per_1k", "export_s_per_1k")}
    entry["runner"] = runner
    entry["commit"] = commit
    entry["recorded"] = f"release build, {run['files']} files, scan-export-scale.yml on {runner} at {commit}"
    return entry


# Top-level marker set by the first real recording (write_baseline). It is
# sticky: once a real scan_export baseline has existed, a placeholder entry
# means the real numbers were lost, and --check fails instead of warning.
REAL_MARKER = "scan_export_real_baseline_recorded"


def write_baseline_file(path, current, entry):
    """Copy of the current baseline file with scan_export replaced by entry."""
    data = dict(current)
    data["scan_export"] = entry
    data[REAL_MARKER] = {"runner": entry["runner"], "commit": entry["commit"]}
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    return data


def baseline_problems(base, run, real_recorded=False):
    """Regressions against base, or None to warn only.

    A placeholder warns (None) until a real baseline has been recorded
    (real_recorded, from the REAL_MARKER in the file). After that a
    placeholder is itself a failure: the 20 percent check cannot be skipped
    by putting the placeholder back."""
    if base.get("placeholder"):
        if real_recorded:
            return ["scan_export is a placeholder, but a real baseline was recorded "
                    "before; restore the recorded entry (the baseline artifact) instead"]
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
    p.add_argument("--baseline-out", help="write a copy of --baseline with this run as scan_export "
                   "(placeholder removed, runner and commit recorded); needs --runner and --commit")
    p.add_argument("--runner", default=os.environ.get("ALBUMEN_RUNNER", ""))
    p.add_argument("--commit", default=os.environ.get("GITHUB_SHA", ""))
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
    rescan_manifest = os.path.join(a.workdir, "rescan.jsonl")
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
        touched = touch_one_percent(library, a.files)
        rescan, rescan_rss = timed([a.albumen, "scan", library, "--manifest", rescan_manifest,
                                    "--ledger", ledger, "--previous", manifest])
        if count_lines(rescan_manifest) != lines:
            raise SystemExit(f"incremental rescan wrote {count_lines(rescan_manifest)} lines, full scan {lines}")
        per_k = 1000.0 / a.files
        run = {
            "files": a.files,
            "scan_s_per_1k": round(scan * per_k, 4),
            "export_s_per_1k": round(export * per_k, 4),
        }
        result = {"generate_s": round(gen, 2), "scan_s": round(scan, 2),
                  "export_s": round(export, 2), "scan_peak_rss_kib": scan_rss,
                  "export_peak_rss_kib": export_rss,
                  "rescan_touched": touched, "rescan_1pct_s": round(rescan, 2),
                  "rescan_1pct_s_per_1k": round(rescan * per_k, 4),
                  "rescan_peak_rss_kib": rescan_rss, **run}
        print(json.dumps(result, indent=2))
        if a.json_out:
            with open(a.json_out, "w", encoding="utf-8") as f:
                json.dump(result, f, indent=2)
                f.write("\n")
        if a.write:
            print(json.dumps({"scan_export": run}, indent=2))
        if a.baseline_out:
            with open(a.baseline, encoding="utf-8") as f:
                current = json.load(f)
            write_baseline_file(a.baseline_out, current, baseline_entry(run, a.runner, a.commit))
            print(f"wrote {a.baseline_out}", file=sys.stderr)
        if a.check:
            with open(a.baseline, encoding="utf-8") as f:
                current = json.load(f)
            base = current.get("scan_export")
            if base is None:
                raise SystemExit(f"{a.baseline} has no scan_export entry")
            problems = baseline_problems(base, run, real_recorded=bool(current.get(REAL_MARKER)))
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
            for path in (os.path.join(a.workdir, "source"), manifest, rescan_manifest, ledger, dest, ENV["ALBUMEN_HOME"]):
                if os.path.isdir(path):
                    shutil.rmtree(path)
                elif os.path.exists(path):
                    os.remove(path)
            os.rmdir(a.workdir)


if __name__ == "__main__":
    main()
