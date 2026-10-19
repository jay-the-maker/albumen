#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail when the quickstart is longer than five steps or drops SHA-256."""

import json
import sys
from pathlib import Path

MAX_STEPS = 5
REQUIRED = ("install", "receive", "upload", "verify")
ROOT = Path(__file__).resolve().parent.parent
QUICKSTART = ROOT / "scripts" / "quickstart.json"
RUNNERS = (
    ROOT / "scripts" / "quickstart.sh",
    ROOT / "scripts" / "quickstart.ps1",
)
INSTALLERS = (
    ROOT / "scripts" / "install.sh",
    ROOT / "scripts" / "install.ps1",
)


def too_many(steps):
    return len(steps) > MAX_STEPS


def check_document(document):
    if document.get("max_steps") != MAX_STEPS:
        raise SystemExit(f"max_steps must be {MAX_STEPS}")
    steps = document.get("steps")
    if not isinstance(steps, list) or not steps:
        raise SystemExit("quickstart steps must be a non-empty list")
    if too_many(steps):
        raise SystemExit(f"quickstart has {len(steps)} steps; the maximum is {MAX_STEPS}")
    ids = []
    for index, step in enumerate(steps, start=1):
        if step.get("n") != index:
            raise SystemExit(f"step {step.get('id')} is numbered {step.get('n')}, expected {index}")
        step_id = step.get("id")
        if not isinstance(step_id, str) or not step_id:
            raise SystemExit("a step is missing id")
        ids.append(step_id)
        for key in ("summary", "posix", "windows"):
            value = step.get(key)
            if not isinstance(value, str) or len(value.strip()) < 8:
                raise SystemExit(f"step {step_id} is missing {key}")
        if "sudo" in step["posix"] or "sudo" in step["windows"]:
            raise SystemExit(f"step {step_id} uses sudo")
    missing = [name for name in REQUIRED if name not in ids]
    if missing:
        raise SystemExit(f"quickstart is missing {', '.join(missing)}")
    verify = next(step for step in steps if step["id"] == "verify")
    blob = " ".join((verify["summary"], verify["posix"], verify["windows"]))
    if "SHA-256" not in blob and "sha256" not in blob.lower():
        raise SystemExit("the verify step does not check SHA-256")
    upload = next(step for step in steps if step["id"] == "upload")
    if "X-Albumen-Mtime" not in upload["posix"] or "X-Albumen-Mtime" not in upload["windows"]:
        raise SystemExit("the upload step is not the phone PUT")
    install = next(step for step in steps if step["id"] == "install")
    if "install.sh" not in install["posix"] or "install.ps1" not in install["windows"]:
        raise SystemExit("the install step does not name both installers")
    return len(steps)


def code_lines(path):
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        lines.append(line)
    return "\n".join(lines)


def check_sources():
    for path in RUNNERS:
        text = path.read_text(encoding="utf-8")
        for step_id in REQUIRED:
            if f"# step: {step_id}" not in text:
                raise SystemExit(f"{path.name} does not run step {step_id}")
        if "X-Albumen-Sha256" not in text and "x-albumen-sha256" not in text.lower():
            raise SystemExit(f"{path.name} does not read X-Albumen-Sha256")
        if "source unchanged" not in text:
            raise SystemExit(f"{path.name} does not check that the source stayed put")
        code = code_lines(path)
        for banned in ("sudo", "cargo", "rustup"):
            if banned in code:
                raise SystemExit(f"{path.name} mentions {banned}")
    for path in INSTALLERS:
        code = code_lines(path)
        for banned in ("sudo", "cargo", "rustup"):
            if banned in code:
                raise SystemExit(f"{path.name} mentions {banned}")


def self_test():
    if not too_many([{}, {}, {}, {}, {}, {}]):
        raise SystemExit("six steps were accepted")
    if too_many([{}, {}, {}, {}]):
        raise SystemExit("four steps were rejected")
    sample = {
        "max_steps": 5,
        "steps": [
            {
                "n": 1,
                "id": "install",
                "summary": "Install the binaries into a home directory.",
                "posix": "sh scripts/install.sh",
                "windows": "powershell -File scripts/install.ps1",
            },
            {
                "n": 2,
                "id": "receive",
                "summary": "Start the local receiver on a library directory.",
                "posix": "albumen-receive --apply",
                "windows": "albumen-receive.exe --apply",
            },
            {
                "n": 3,
                "id": "upload",
                "summary": "PUT one original with the phone header.",
                "posix": "curl -H X-Albumen-Mtime: 1 http://127.0.0.1",
                "windows": "curl.exe -H X-Albumen-Mtime: 1 http://127.0.0.1",
            },
            {
                "n": 4,
                "id": "verify",
                "summary": "Compare the SHA-256 of the copy with the source.",
                "posix": "sha256sum source copy",
                "windows": "Get-FileHash -Algorithm SHA256 source",
            },
        ],
    }
    if check_document(sample) != 4:
        raise SystemExit("self-test count was not 4")
    sample["steps"].append(dict(sample["steps"][-1], n=5, id="extra"))
    sample["steps"].append(dict(sample["steps"][-1], n=6, id="sixth"))
    try:
        check_document(sample)
    except SystemExit as err:
        if "maximum" not in str(err):
            raise
    else:
        raise SystemExit("self-test accepted six steps")


def main():
    if "--self-test" in sys.argv:
        self_test()
        print("quickstart self-test: ok")
        return
    document = json.loads(QUICKSTART.read_text(encoding="utf-8"))
    count = check_document(document)
    check_sources()
    print(f"quickstart steps: {count}")


if __name__ == "__main__":
    main()
