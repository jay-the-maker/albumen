#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Print a read-only fingerprint of a library: one line per entry with
kind, size, mtime_ns, mode and SHA-256 (symlinks are not followed), then a
final `total` line with the SHA-256 of all lines.

    fingerprint.py LIB > before.sha   # before driving the GUI
    fingerprint.py LIB > after.sha    # after
    cmp before.sha after.sha          # must be identical
"""
import hashlib
import os
import sys


def main() -> int:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = sys.argv[1]
    lines = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        for name in sorted(dirnames + filenames):
            p = os.path.join(dirpath, name)
            st = os.lstat(p)
            rel = os.path.relpath(p, root).encode("unicode_escape").decode()
            if os.path.islink(p):
                kind = "link->" + os.readlink(p)
            elif os.path.isdir(p):
                kind = "dir"
            else:
                h = hashlib.sha256()
                with open(p, "rb") as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                kind = h.hexdigest()
            lines.append(f"{rel}\t{kind}\t{st.st_size}\t{st.st_mtime_ns}\t{oct(st.st_mode)}")
    body = "\n".join(lines)
    print(body)
    print(f"total\t{len(lines)} entries\t{hashlib.sha256(body.encode()).hexdigest()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
