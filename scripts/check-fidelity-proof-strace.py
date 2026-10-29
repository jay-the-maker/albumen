#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Read strace -f logs and fail on any network syscall, or on any write,
rename, delete, permission, time, or xattr change under a protected root.

    check-fidelity-proof-strace.py LOG... -- ROOT...
    check-fidelity-proof-strace.py --self-test
"""

import sys
from pathlib import Path

NETWORK = ("socket(", "connect(", "bind(", "listen(", "accept(", "accept4(",
           "sendto(", "sendmsg(", "sendmmsg(", "recvfrom(", "recvmsg(")
MUTATE = ("unlink(", "unlinkat(", "rename(", "renameat(", "renameat2(",
          "truncate(", "chmod(", "fchmodat(", "chown(", "fchownat(", "lchown(",
          "setxattr(", "lsetxattr(", "removexattr(", "lremovexattr(",
          "utimensat(", "utimes(", "mkdir(", "mkdirat(", "rmdir(", "link(",
          "linkat(", "symlink(", "symlinkat(", "mknod(", "mknodat(")
WRITE_FLAGS = ("O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND")


def under(line, root):
    """True when root is a path prefix in line, not a sibling like root2."""
    start = 0
    while True:
        at = line.find(root, start)
        if at < 0:
            return False
        end = at + len(root)
        if end == len(line) or line[end] in "/\"' ,)}":
            return True
        start = at + 1


def problems(text, roots):
    found = []
    for line in text.splitlines():
        head = line.split("=", 1)[0]
        call = head.split(None, 1)[-1] if " " in head else head
        if any(call.startswith(n) for n in NETWORK):
            found.append(f"network syscall: {line}")
            continue
        hit = [r for r in roots if under(line, r)]
        if not hit:
            continue
        if any(call.startswith(n) for n in MUTATE):
            found.append(f"change under {hit[0]}: {line}")
        elif call.startswith(("open(", "openat(", "openat2(", "creat(")) and any(
            f in line for f in WRITE_FLAGS
        ):
            found.append(f"write open under {hit[0]}: {line}")
    return found


def self_test():
    roots = ["/w/library"]
    clean = "\n".join([
        '10 openat(AT_FDCWD, "/w/library/a.jpg", O_RDONLY|O_CLOEXEC) = 3',
        '10 openat(AT_FDCWD, "/w/out/proof.json", O_WRONLY|O_CREAT|O_EXCL, 0644) = 4',
        '10 openat(AT_FDCWD, "/w/library2/x", O_WRONLY|O_CREAT, 0644) = 5',
        '10 socketpair(AF_UNIX, SOCK_STREAM, 0, [3, 4]) = 0',
        '10 +++ exited with 0 +++',
    ])
    if problems(clean, roots):
        raise SystemExit(f"self-test: clean log flagged: {problems(clean, roots)}")
    for bad in [
        '11 socket(AF_INET, SOCK_STREAM, IPPROTO_TCP) = 3',
        '11 connect(3, {sa_family=AF_INET6, sin6_port=htons(443)}, 28) = 0',
        '11 openat(AT_FDCWD, "/w/library/a.jpg", O_RDWR) = 3',
        '11 renameat2(AT_FDCWD, "/w/library/a", AT_FDCWD, "/w/library/b", 0) = 0',
        '11 unlink("/w/library/a.jpg") = 0',
        '11 utimensat(AT_FDCWD, "/w/library/a.jpg", NULL, 0) = 0',
    ]:
        if not problems(bad, roots):
            raise SystemExit(f"self-test: missed {bad}")
    print("check-fidelity-proof-strace: self-test ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return
    if "--" not in argv:
        raise SystemExit(__doc__)
    split = argv.index("--")
    logs, roots = argv[:split], [str(Path(r).resolve()) for r in argv[split + 1:]]
    if not logs or not roots:
        raise SystemExit(__doc__)
    bad = []
    for log in logs:
        bad += [f"{log}: {p}" for p in problems(Path(log).read_text(errors="replace"), roots)]
    if bad:
        raise SystemExit("\n".join(bad))
    print(f"strace: {len(logs)} logs, no network syscalls, no changes under {len(roots)} roots")


if __name__ == "__main__":
    main(sys.argv[1:])
