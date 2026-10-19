#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail when production code writes a file outside ingest/src/dest.rs.

Sources are read-only. The destination module is the only place that may
create, write, rename, or remove a file. Test modules are ignored. The bench
harness is ignored because it writes synthetic timing files and never opens a
user archive. receive/src/roundtrip.rs is a test-only module.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SCAN_ROOTS = [
    ROOT / "ingest" / "src",
    ROOT / "cli" / "src",
    ROOT / "mcp" / "src",
    ROOT / "receive" / "src",
]

SKIP = {
    (ROOT / "ingest" / "src" / "dest.rs").resolve(),
    (ROOT / "receive" / "src" / "roundtrip.rs").resolve(),
}

PATTERNS = [
    re.compile(r"\bfs::write\b"),
    re.compile(r"\bstd::fs::write\b"),
    re.compile(r"\bfs::remove_file\b"),
    re.compile(r"\bstd::fs::remove_file\b"),
    re.compile(r"\bfs::remove_dir\b"),
    re.compile(r"\bstd::fs::remove_dir\b"),
    re.compile(r"\bfs::remove_dir_all\b"),
    re.compile(r"\bstd::fs::remove_dir_all\b"),
    re.compile(r"\bfs::rename\b"),
    re.compile(r"\bstd::fs::rename\b"),
    re.compile(r"\bfs::create_dir\b"),
    re.compile(r"\bstd::fs::create_dir\b"),
    re.compile(r"\bfs::create_dir_all\b"),
    re.compile(r"\bstd::fs::create_dir_all\b"),
    re.compile(r"\bFile::create\b"),
    re.compile(r"\.write\(true\)"),
    re.compile(r"\.create\(true\)"),
    re.compile(r"\.create_new\(true\)"),
    re.compile(r"\.append\(true\)"),
    re.compile(r"\bset_times\b"),
    re.compile(r"\bset_permissions\b"),
    re.compile(r"\bxattr::set\b"),
]


def matching_brace(src: str, open_index: int) -> int:
    depth = 0
    i = open_index
    n = len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            newline = src.find("\n", i)
            i = n if newline < 0 else newline
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            end = src.find("*/", i + 2)
            i = n if end < 0 else end + 2
            continue
        if c in {'"', "'"}:
            quote = c
            i += 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                    continue
                if src[i] == quote:
                    i += 1
                    break
                i += 1
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def strip_test_modules(src: str) -> str:
    out: list[str] = []
    i = 0
    n = len(src)
    marker = "#[cfg(test)]"
    while i < n:
        if src.startswith(marker, i):
            k = i + len(marker)
            while k < n and src[k].isspace():
                k += 1
            if src.startswith("mod", k) and (k + 3 == n or not src[k + 3].isalnum()):
                brace = src.find("{", k)
                if brace < 0:
                    out.append(src[i:])
                    break
                i = matching_brace(src, brace)
                continue
        out.append(src[i])
        i += 1
    return "".join(out)


def scan_file(path: Path) -> list[str]:
    text = strip_test_modules(path.read_text(encoding="utf-8"))
    hits: list[str] = []
    for number, line in enumerate(text.splitlines(), start=1):
        stripped = line.split("//", 1)[0]
        for pattern in PATTERNS:
            if pattern.search(stripped):
                hits.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
                break
    return hits


def production_hits() -> list[str]:
    hits: list[str] = []
    for root in SCAN_ROOTS:
        for path in sorted(root.rglob("*.rs")):
            if path.resolve() in SKIP:
                continue
            hits.extend(scan_file(path))
    return hits


def self_test() -> None:
    sample = """
fn real() {
    std::fs::write(\"nope\", b\"x\");
}
#[cfg(test)]
mod tests {
    fn fixture() {
        std::fs::write(\"ok\", b\"x\");
    }
}
"""
    stripped = strip_test_modules(sample)
    if "std::fs::write(\"ok\"" in stripped:
        raise SystemExit("self-test: test module was not stripped")
    if "std::fs::write(\"nope\"" not in stripped:
        raise SystemExit("self-test: production write was stripped")
    allowed = strip_test_modules(
        """
fn ok() {
    dest::remove_file(path)?;
    file.write_all(bytes)?;
}
"""
    )
    for pattern in PATTERNS:
        if pattern.search(allowed):
            raise SystemExit(f"self-test: allowed destination call matched {pattern.pattern}")


def main() -> int:
    if "--self-test" in sys.argv:
        self_test()
        print("check-dest-writes self-test ok")
        return 0
    hits = production_hits()
    if hits:
        print("raw filesystem writes belong in ingest/src/dest.rs:", file=sys.stderr)
        for hit in hits:
            print(hit, file=sys.stderr)
        return 1
    print("destination writes stay in ingest/src/dest.rs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
