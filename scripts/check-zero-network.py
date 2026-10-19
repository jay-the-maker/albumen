#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail when an strace shows an IPv4 or IPv6 connection attempt.

DNS is a sendto or connect. A connect aimed at an external address, including
one made from a socket bound to loopback, is a connection attempt. AF_UNIX is
not a network connection.
"""

import re
import sys

SOCKET = re.compile(r"\bsocket\(AF_INET6?\b")
OPERATION = re.compile(
    r"\b(connect|sendto|sendmsg|sendmmsg|bind|listen|accept4?|recvfrom|recvmsg)\("
)
INET = re.compile(r"AF_INET6?\b|sin6?_addr=|(?:TCP|UDP|RAW):\[")


def connection_attempts(text):
    found = []
    for line in text.splitlines():
        if SOCKET.search(line) or (OPERATION.search(line) and INET.search(line)):
            found.append(line)
    return found


def self_test():
    clean = "\n".join(
        [
            '10 socket(AF_UNIX, SOCK_STREAM, 0) = 3',
            '10 connect(3, {sa_family=AF_UNIX, sun_path="/tmp/x"}, 16) = 0',
            "10 +++ exited with 0 +++",
        ]
    )
    if connection_attempts(clean):
        raise SystemExit("a unix socket was treated as a network connection")
    samples = "\n".join(
        [
            '11 socket(AF_INET, SOCK_STREAM, IPPROTO_TCP) = 4',
            '11 connect(4, {sa_family=AF_INET, sin_port=htons(9), sin_addr=inet_addr("203.0.113.5")}, 16) = -1',
            '12 connect(5<TCP:[127.0.0.1:40000->127.0.0.1:9]>, {sa_family=AF_INET, sin_port=htons(9), sin_addr=inet_addr("127.0.0.1")}, 16) = -1',
            '13 sendto(6<UDP:[0.0.0.0:1->127.0.0.1:53]>, "x", 1, 0, NULL, 0) = 1',
            '14 socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP) = 7',
            '14 sendto(7, "x", 1, 0, {sa_family=AF_INET6, sin6_port=htons(53), inet_pton(AF_INET6, "2001:db8::1", &sin6_addr)}, 28) = 1',
        ]
    )
    found = connection_attempts(samples)
    if len(found) != 6:
        raise SystemExit(f"self-test saw {len(found)} attempts, expected 6")
    blob = "\n".join(found)
    for needle in ("203.0.113.5", "127.0.0.1", ":53", "2001:db8::1"):
        if needle not in blob:
            raise SystemExit(f"self-test missed {needle}")
    print("zero-network self-test: ok")


def main(argv):
    if "--self-test" in argv:
        self_test()
        return
    expect_clean = "--expect-clean" in argv
    expect_attempts = "--expect-attempts" in argv
    if expect_clean == expect_attempts:
        raise SystemExit("pass exactly one of --expect-clean or --expect-attempts")
    contains = []
    path = None
    args = argv[1:]
    index = 0
    while index < len(args):
        arg = args[index]
        if arg == "--contains":
            index += 1
            contains.append(args[index])
        elif arg in ("--expect-clean", "--expect-attempts"):
            pass
        elif arg.startswith("--"):
            raise SystemExit(f"unknown argument {arg}")
        else:
            path = arg
        index += 1
    if path is None:
        raise SystemExit("pass the strace path")
    text = open(path, encoding="utf-8", errors="replace").read()
    found = connection_attempts(text)
    blob = "\n".join(found)
    if expect_clean:
        if found:
            preview = "\n".join(found[:20])
            raise SystemExit(f"{path}: connection attempt\n{preview}")
        print(f"zero-network: no connection attempts in {path}")
        return
    missing = [needle for needle in contains if needle not in blob]
    if not found or missing:
        raise SystemExit(
            f"{path}: observer missed a connection attempt "
            f"(lines={len(found)}, missing={missing or 'none'})"
        )
    print(f"zero-network: observer saw {len(found)} connection attempts in {path}")


if __name__ == "__main__":
    main(sys.argv)
