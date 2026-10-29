#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Audit helpers for scripts/fidelity-proof-no-network.sh.

    check-fidelity-proof-strace.py [--cwd DIR] LOG... -- ROOT...
        Read strace -f -y logs and fail on any network syscall, or on any
        write, rename, delete, permission, time, or xattr change under a
        protected root. Path arguments, fd arguments (shown by -y as
        3</path>), relative paths and AT_FDCWD are all resolved. Relative
        paths start from --cwd (default /) and follow chdir and fchdir.
    check-fidelity-proof-strace.py snap OUT ROOT...
        Write a snapshot of every entry under the roots (kind, size, sha256,
        mode) to OUT as JSON.
    check-fidelity-proof-strace.py compare LABEL BEFORE AFTER [EXPECT...]
        Fail unless the paths that were added, removed or changed between the
        two snapshots are exactly EXPECT.
    check-fidelity-proof-strace.py --self-test
"""

import hashlib
import json
import os
import re
import stat
import sys
import tempfile
from pathlib import Path

# Exact syscall names. The check stays strict: any socket, even AF_UNIX,
# fails the audit. socketpair is not a network call.
NETWORK = {"socket", "connect", "bind", "listen", "accept", "accept4",
           "sendto", "sendmsg", "sendmmsg", "recvfrom", "recvmsg"}
WRITE_FLAGS = ("O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND")

# Calls that change what their first argument names. "fd" means the argument
# is a descriptor that strace -y prints as N</path>.
FD_CALLS = {name: [0] for name in (
    "write", "pwrite64", "writev", "pwritev", "pwritev2", "ftruncate",
    "ftruncate64", "fchmod", "fchown", "fchown32", "fsetxattr",
    "fremovexattr", "fallocate", "sendfile", "sendfile64")}
FD_CALLS.update({"copy_file_range": [2], "splice": [2]})
# name: list of (dirfd index or None, path index)
PATH_CALLS = {
    "open": [(None, 0)], "creat": [(None, 0)], "truncate": [(None, 0)],
    "truncate64": [(None, 0)], "unlink": [(None, 0)], "rmdir": [(None, 0)],
    "mkdir": [(None, 0)], "mknod": [(None, 0)], "chmod": [(None, 0)],
    "chown": [(None, 0)], "lchown": [(None, 0)], "utime": [(None, 0)],
    "utimes": [(None, 0)], "setxattr": [(None, 0)], "lsetxattr": [(None, 0)],
    "removexattr": [(None, 0)], "lremovexattr": [(None, 0)],
    "rename": [(None, 0), (None, 1)], "link": [(None, 1)],
    "symlink": [(None, 1)],
    "openat": [(0, 1)], "openat2": [(0, 1)], "unlinkat": [(0, 1)],
    "mkdirat": [(0, 1)], "mknodat": [(0, 1)], "fchmodat": [(0, 1)],
    "fchmodat2": [(0, 1)], "fchownat": [(0, 1)], "utimensat": [(0, 1)],
    "futimesat": [(0, 1)], "renameat": [(0, 1), (2, 3)],
    "renameat2": [(0, 1), (2, 3)], "linkat": [(2, 3)], "symlinkat": [(1, 2)],
}
OPENS = {"open", "openat", "openat2", "creat"}
FD_PATH = re.compile(r"^(?:-?\d+|AT_FDCWD)<(.*)>$")
LINE = re.compile(r"^(\d+)?\s*([a-z_][a-z0-9_]*)\((.*)$")


def split_args(text):
    """Split a syscall's argument text at top-level commas, up to its ')'."""
    args, depth, cur, quote, esc = [], 0, [], False, False
    for ch in text:
        if quote:
            cur.append(ch)
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                quote = False
            continue
        if ch == '"':
            quote = True
        elif ch in "([{<":
            depth += 1
        elif ch in ")]}>":
            if depth == 0 and ch == ")":
                break
            depth -= 1
        elif ch == "," and depth == 0:
            args.append("".join(cur).strip())
            cur = []
            continue
        cur.append(ch)
    tail = "".join(cur).strip()
    if tail.endswith("<unfinished ..."):
        tail = tail[: -len("<unfinished ...")].strip()
    if tail:
        args.append(tail)
    return args


def unquote(arg):
    if len(arg) >= 2 and arg.startswith('"'):
        body = arg[1:].split('"', 1)[0]
        return body.encode("latin-1", "backslashreplace").decode("unicode_escape")
    return None


def fd_path(arg):
    match = FD_PATH.match(arg.strip()) if arg else None
    return match.group(1) if match else None


def resolve(path, base):
    return os.path.normpath(path if path.startswith("/") else os.path.join(base, path))


def under(path, root):
    return path == root or path.startswith(root.rstrip("/") + "/")


def problems(text, roots, cwd="/"):
    roots = [os.path.normpath(r) for r in roots]
    cwds = {}
    found = []
    for line in text.splitlines():
        match = LINE.match(line.strip())
        if not match or "resumed>" in line:
            continue
        pid, name, rest = match.group(1) or "0", match.group(2), match.group(3)
        if name in NETWORK:
            found.append(f"network syscall: {line}")
            continue
        args = split_args(rest)
        here = cwds.get(pid, cwd)
        failed = line.rstrip().split("=")[-1].strip().startswith("-1")
        if name == "chdir" and args and not failed:
            cwds[pid] = resolve(unquote(args[0]) or "", here)
            continue
        if name == "fchdir" and args and not failed and fd_path(args[0]):
            cwds[pid] = fd_path(args[0])
            continue
        targets = []
        for index in FD_CALLS.get(name, []):
            if index < len(args) and fd_path(args[index]):
                targets.append(fd_path(args[index]))
        for dir_index, path_index in PATH_CALLS.get(name, []):
            if path_index >= len(args):
                continue
            path = unquote(args[path_index])
            base = here
            if dir_index is not None and dir_index < len(args):
                base = fd_path(args[dir_index]) or here
            if path is None:
                # utimensat(fd, NULL, ...) is futimens on the fd itself.
                if dir_index is not None and fd_path(args[dir_index]):
                    targets.append(fd_path(args[dir_index]))
                continue
            targets.append(resolve(path, base))
        if name in OPENS:
            flags = " ".join(args[1:]) if name != "openat2" else " ".join(args[2:])
            if name == "creat":
                flags = "O_CREAT"
            if not any(f in flags for f in WRITE_FLAGS):
                continue
            kind = "write open"
        else:
            kind = "change"
        for target in targets:
            hit = [r for r in roots if under(target, r)]
            if hit:
                found.append(f"{kind} under {hit[0]}: {line}")
                break
    return found


def entry(path):
    st = os.lstat(path)
    if stat.S_ISLNK(st.st_mode):
        return ["l", 0, os.readlink(path), st.st_mode]
    if stat.S_ISDIR(st.st_mode):
        return ["d", 0, "", st.st_mode]
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return ["f", st.st_size, digest.hexdigest(), st.st_mode]


def snapshot(roots):
    out = {}
    for root in roots:
        root = os.path.realpath(root)
        if not os.path.lexists(root):
            continue
        out[root] = entry(root)
        for top, dirs, files in os.walk(root):
            for name in dirs + files:
                path = os.path.join(top, name)
                out[path] = entry(path)
    return out


def changes(before, after):
    return {p for p in set(before) | set(after) if before.get(p) != after.get(p)}


def unexpected(before, after, expect):
    expect = {os.path.realpath(p) for p in expect}
    changed = changes(before, after)
    bad = [f"unexpected change: {p} (before {before.get(p)}, after {after.get(p)})"
           for p in sorted(changed - expect)]
    bad += [f"expected output not written: {p}" for p in sorted(expect - changed)]
    return bad


def self_test():
    # strace reader: the original cases.
    roots = ["/w/library"]
    clean = "\n".join([
        '10 openat(AT_FDCWD, "/w/library/a.jpg", O_RDONLY|O_CLOEXEC) = 3',
        '10 openat(AT_FDCWD, "/w/out/proof.json", O_WRONLY|O_CREAT|O_EXCL, 0644) = 4',
        '10 openat(AT_FDCWD, "/w/library2/x", O_WRONLY|O_CREAT, 0644) = 5',
        '10 socketpair(AF_UNIX, SOCK_STREAM, 0, [3, 4]) = 0',
        '10 read(3</w/library/a.jpg>, "\\377\\330", 65536) = 2',
        '10 write(1</dev/pts/0>, "verified /w/library/a.jpg\\n", 26) = 26',
        '10 write(4</w/out/proof.json>, "{}", 2) = 2',
        '10 openat(AT_FDCWD</w>, "library/a.jpg", O_RDONLY) = 6',
        '10 utimensat(4</w/out/proof.json>, NULL, NULL, 0) = 0',
        '10 fstat(3</w/library/a.jpg>, {st_mode=S_IFREG|0444, st_size=2}) = 0',
        '10 +++ exited with 0 +++',
    ])
    if problems(clean, roots, "/w"):
        raise SystemExit(f"self-test: clean log flagged: {problems(clean, roots, '/w')}")
    bad_lines = [
        '11 socket(AF_INET, SOCK_STREAM, IPPROTO_TCP) = 3',
        '11 socket(AF_UNIX, SOCK_STREAM|SOCK_CLOEXEC, 0) = 3',
        '11 connect(3<socket:[1]>, {sa_family=AF_INET6, sin6_port=htons(443)}, 28) = 0',
        '11 openat(AT_FDCWD, "/w/library/a.jpg", O_RDWR) = 3',
        '11 renameat2(AT_FDCWD, "/w/library/a", AT_FDCWD, "/w/library/b", 0) = 0',
        '11 unlink("/w/library/a.jpg") = 0',
        '11 utimensat(AT_FDCWD, "/w/library/a.jpg", NULL, 0) = 0',
        # fd-based calls on a protected file
        '11 write(3</w/library/a.jpg>, "x", 1) = 1',
        '11 pwrite64(3</w/library/a.jpg>, "x", 1, 0) = 1',
        '11 ftruncate(3</w/library/a.jpg>, 0) = 0',
        '11 fchmod(3</w/library/a.jpg>, 0644) = 0',
        '11 fchown(3</w/library/a.jpg>, 0, 0) = 0',
        '11 fsetxattr(3</w/library/a.jpg>, "user.x", "1", 1, 0) = 0',
        '11 utimensat(3</w/library/a.jpg>, NULL, NULL, 0) = 0',
        # relative paths: against the dirfd, AT_FDCWD's path, and the cwd
        '11 openat(5</w/library>, "a.jpg", O_RDWR) = 3',
        '11 openat(AT_FDCWD</w/library>, "a.jpg", O_WRONLY|O_TRUNC) = 3',
        '11 openat(AT_FDCWD, "library/a.jpg", O_WRONLY) = 3',
        '11 unlinkat(AT_FDCWD, "library/../library/a.jpg", 0) = 0',
        '12 openat(AT_FDCWD, "library/a.jpg", O_WRONLY <unfinished ...>',
    ]
    for bad in bad_lines:
        if not problems(bad, roots, "/w"):
            raise SystemExit(f"self-test: missed {bad}")
    chdir = '11 chdir("library") = 0\n11 openat(AT_FDCWD, "a.jpg", O_WRONLY) = 3'
    if not problems(chdir, roots, "/w"):
        raise SystemExit("self-test: missed a write after chdir")
    if problems('11 openat(AT_FDCWD, "a.jpg", O_RDONLY) = 3', roots, "/w/library"):
        raise SystemExit("self-test: read-only relative open flagged")

    # snapshots
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, "library")
        out = os.path.join(tmp, "out")
        os.makedirs(src)
        os.makedirs(out)
        photo = os.path.join(src, "a.jpg")
        Path(photo).write_bytes(b"synthetic photo bytes")
        before = snapshot([tmp])
        if unexpected(before, snapshot([tmp]), set()):
            raise SystemExit("self-test: untouched tree flagged")

        # tampered source: same size, one byte changed
        Path(photo).write_bytes(b"synthetic photo bytez")
        if not unexpected(before, snapshot([tmp]), set()):
            raise SystemExit("self-test: tampered source not caught")
        Path(photo).write_bytes(b"synthetic photo bytes")
        if unexpected(before, snapshot([tmp]), set()):
            raise SystemExit("self-test: restored source still flagged")

        # mode-only change
        os.chmod(photo, 0o600)
        if not unexpected(before, snapshot([tmp]), set()):
            raise SystemExit("self-test: chmod not caught")
        os.chmod(photo, stat.S_IMODE(before[os.path.realpath(photo)][3]))

        # the expected output, then a stray extra write next to it
        proof = os.path.join(out, "proof.json")
        Path(proof).write_text("{}")
        if unexpected(before, snapshot([tmp]), {proof}):
            raise SystemExit("self-test: expected output flagged")
        stray = os.path.join(tmp, "stray.tmp")
        Path(stray).write_text("x")
        bad = unexpected(before, snapshot([tmp]), {proof})
        if not any(os.path.realpath(stray) in b for b in bad):
            raise SystemExit(f"self-test: stray write not caught: {bad}")
        os.remove(stray)

        # an expected output that was never written
        if not unexpected(before, snapshot([tmp]), {proof, os.path.join(out, "proof.md")}):
            raise SystemExit("self-test: missing output not caught")
    print("check-fidelity-proof-strace: self-test ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return
    if argv[:1] == ["snap"] and len(argv) >= 3:
        Path(argv[1]).write_text(json.dumps(snapshot(argv[2:]), sort_keys=True))
        return
    if argv[:1] == ["compare"] and len(argv) >= 4:
        label, before, after = argv[1], json.loads(Path(argv[2]).read_text()), json.loads(Path(argv[3]).read_text())
        bad = unexpected(before, after, set(argv[4:]))
        if bad:
            raise SystemExit("\n".join(f"{label}: {b}" for b in bad))
        print(f"{label}: {len(after)} entries, only the expected outputs changed ({len(argv) - 4})")
        return
    cwd = "/"
    if argv[:1] == ["--cwd"] and len(argv) >= 2:
        cwd, argv = argv[1], argv[2:]
    if "--" not in argv:
        raise SystemExit(__doc__)
    split = argv.index("--")
    logs, roots = argv[:split], [str(Path(r).resolve()) for r in argv[split + 1:]]
    if not logs or not roots:
        raise SystemExit(__doc__)
    bad = []
    for log in logs:
        bad += [f"{log}: {p}" for p in problems(Path(log).read_text(errors="replace"), roots, cwd)]
    if bad:
        raise SystemExit("\n".join(bad))
    print(f"strace: {len(logs)} logs, no network syscalls, no changes under {len(roots)} roots")


if __name__ == "__main__":
    main(sys.argv[1:])
