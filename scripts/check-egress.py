#!/usr/bin/env python3
"""Fail when a strace shows a network destination the command did not name.

Allow-lists are comma-separated. An empty connect list means every IPv4 and
IPv6 connect, sendto, or sendmsg is unexpected. Bind to port 0 is the kernel's
ephemeral port and is always allowed. Any other bound port must be listed.
"""

import re
import sys

SYSCALL = re.compile(
    r"\b(connect|sendto|sendmsg|bind)\(.*sa_family=(AF_INET6?)"
    r".*?(?:sin_port|sin6_port)=htons\((\d+)\)"
    r".*?(?:inet_addr\(\"([^\"]+)\"\)|inet_pton\(AF_INET6?, \"([^\"]+)\")"
)


def parse_list(text, kind):
    if not text.strip():
        return set()
    items = set()
    for part in text.split(","):
        part = part.strip()
        if not part:
            continue
        if kind == "connect":
            host, port = part.rsplit(":", 1)
            items.add((normalize(host), int(port)))
        else:
            items.add(int(part))
    return items


def normalize(host):
    if host.startswith("::ffff:"):
        return host.removeprefix("::ffff:")
    return host


def inspect(text, allow_connect, allow_bind):
    problems = []
    allowed_connects = 0
    allowed_binds = 0
    for line in text.splitlines():
        match = SYSCALL.search(line)
        if match is None:
            continue
        call, _family, port_text, v4, v6 = match.groups()
        host = normalize(v4 or v6)
        port = int(port_text)
        if call == "bind":
            if port == 0 or port in allow_bind:
                if port in allow_bind:
                    allowed_binds += 1
                continue
            problems.append(line)
            continue
        if (host, port) in allow_connect:
            allowed_connects += 1
            continue
        problems.append(line)
    if allow_connect and allowed_connects == 0:
        problems.append("configured destination was not contacted")
    if allow_bind and allowed_binds == 0:
        problems.append("configured listen address was not bound")
    return problems


def self_test():
    allowed = inspect(
        '1 connect(5, {sa_family=AF_INET, sin_port=htons(19000), sin_addr=inet_addr("127.0.0.1")}, 16) = 0\n'
        '1 bind(6, {sa_family=AF_INET, sin_port=htons(0), sin_addr=inet_addr("0.0.0.0")}, 16) = 0\n'
        "1 connect(7, {sa_family=AF_UNIX, sun_path=\"/tmp/x\"}, 12) = 0\n",
        {("127.0.0.1", 19000)},
        set(),
    )
    if allowed:
        raise SystemExit(f"self-test: allowed connect was rejected: {allowed}")
    missed = inspect("no sockets here\n", {("127.0.0.1", 19000)}, set())
    if "configured destination was not contacted" not in missed:
        raise SystemExit("self-test: empty trace did not require the destination")
    blocked = inspect(
        '1 connect(5, {sa_family=AF_INET, sin_port=htons(443), sin_addr=inet_addr("1.1.1.1")}, 16) = 0\n'
        '1 sendto(6, "", 0, 0, {sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("8.8.8.8")}, 16) = 0\n',
        set(),
        set(),
    )
    if len(blocked) != 2:
        raise SystemExit(f"self-test: unexpected destinations were not all reported: {blocked}")
    bound = inspect(
        '1 bind(3, {sa_family=AF_INET, sin_port=htons(18080), sin_addr=inet_addr("127.0.0.1")}, 16) = 0\n',
        set(),
        {18080},
    )
    if bound:
        raise SystemExit(f"self-test: configured bind was rejected: {bound}")
    other_bind = inspect(
        '1 bind(3, {sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("0.0.0.0")}, 16) = 0\n',
        set(),
        {18080},
    )
    if not other_bind:
        raise SystemExit("self-test: unbound listen port was accepted")
    print("check-egress: self-test ok")


def main(argv):
    if argv[1:] == ["--self-test"]:
        self_test()
        return 0
    if len(argv) != 5:
        print(
            "usage: check-egress.py TRACE ALLOW_CONNECT ALLOW_BIND NAME",
            file=sys.stderr,
        )
        return 2
    trace, allow_connect, allow_bind, name = argv[1:]
    text = open(trace, errors="replace").read()
    problems = inspect(
        text, parse_list(allow_connect, "connect"), parse_list(allow_bind, "bind")
    )
    if problems:
        print(f"egress: {name} reached a destination it was not given", file=sys.stderr)
        for problem in problems:
            print(problem, file=sys.stderr)
        return 1
    print(f"egress: {name} stayed on the destinations it was given")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
