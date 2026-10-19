#!/usr/bin/env python3
"""Fail when a traced destination is missing from the ledger.

The ledger is local. This check reads strace output and the ledger JSON. A
connect, sendto, or sendmsg to host:port must appear as a connect destination.
A bind to a port other than 0 must appear as a bind destination. Port 0 and
AF_UNIX are not destinations. A destination in the ledger that the trace did
not show does not fail the run.
"""

import json
import re
import sys
from pathlib import Path

SYSCALL = re.compile(
    r"\b(connect|sendto|sendmsg|bind)\(.*sa_family=(AF_INET6?)"
    r".*?(?:sin_port|sin6_port)=htons\((\d+)\)"
    r".*?(?:inet_addr\(\"([^\"]+)\"\)|inet_pton\(AF_INET6?, \"([^\"]+)\")"
)


def normalize(host):
    if host.startswith("::ffff:"):
        return host.removeprefix("::ffff:")
    return host


def observed(text):
    connects = set()
    binds = set()
    for line in text.splitlines():
        match = SYSCALL.search(line)
        if match is None:
            continue
        call, _family, port_text, v4, v6 = match.groups()
        host = normalize(v4 or v6)
        port = int(port_text)
        if call == "bind":
            if port != 0:
                binds.add((host, port))
            continue
        connects.add((host, port))
    return connects, binds


def disclosed(ledger):
    connects = set()
    binds = set()
    for destination in ledger.get("destinations", []):
        host = normalize(str(destination["host"]))
        port = int(destination["port"])
        kind = destination["kind"]
        if kind == "bind":
            binds.add((host, port))
        elif kind == "connect":
            connects.add((host, port))
    return connects, binds


def missing(connects, binds, ledger):
    have_connects, have_binds = disclosed(ledger)
    undisclosed = []
    for host, port in sorted(connects - have_connects):
        undisclosed.append(f"connect {host}:{port}")
    for host, port in sorted(binds - have_binds):
        undisclosed.append(f"bind {host}:{port}")
    return undisclosed


def load_ledger(path):
    text = Path(path).read_text(errors="replace")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        start = text.find("{")
        end = text.rfind("}")
        if start < 0 or end < start:
            raise SystemExit(f"ledger JSON is missing in {path}")
        return json.loads(text[start : end + 1])


def gather(trace):
    path = Path(trace)
    files = [path] if path.is_file() else sorted(path.glob("*.strace"))
    connects = set()
    binds = set()
    for file in files:
        more_connects, more_binds = observed(file.read_text(errors="replace"))
        connects |= more_connects
        binds |= more_binds
    return connects, binds


def self_test():
    trace = (
        '1 connect(5, {sa_family=AF_INET, sin_port=htons(19000), sin_addr=inet_addr("127.0.0.1")}, 16) = 0\n'
        '1 bind(6, {sa_family=AF_INET, sin_port=htons(0), sin_addr=inet_addr("0.0.0.0")}, 16) = 0\n'
        '1 bind(7, {sa_family=AF_INET, sin_port=htons(18080), sin_addr=inet_addr("127.0.0.1")}, 16) = 0\n'
        '1 connect(8, {sa_family=AF_UNIX, sun_path="/tmp/x"}, 12) = 0\n'
    )
    connects, binds = observed(trace)
    ledger = {
        "destinations": [
            {"via": "export", "kind": "connect", "host": "127.0.0.1", "port": 19000},
            {"via": "receive", "kind": "bind", "host": "127.0.0.1", "port": 18080},
            {"via": "export", "kind": "connect", "host": "10.0.0.1", "port": 443},
        ]
    }
    if missing(connects, binds, ledger):
        raise SystemExit(f"self-test: disclosed destinations were rejected: {missing(connects, binds, ledger)}")
    hidden = missing(
        connects,
        binds,
        {"destinations": [{"via": "export", "kind": "connect", "host": "127.0.0.1", "port": 19000}]},
    )
    if "bind 127.0.0.1:18080" not in hidden:
        raise SystemExit(f"self-test: undisclosed bind was accepted: {hidden}")
    surprise = missing(
        {("1.1.1.1", 443)},
        set(),
        {"destinations": []},
    )
    if surprise != ["connect 1.1.1.1:443"]:
        raise SystemExit(f"self-test: undisclosed connect was accepted: {surprise}")
    if observed("1 connect(7, {sa_family=AF_UNIX, sun_path=\"/tmp/x\"}, 12) = 0\n") != (set(), set()):
        raise SystemExit("self-test: AF_UNIX was treated as a destination")
    print("check-ledger-egress: self-test ok")


def main(argv):
    if argv[1:] == ["--self-test"]:
        self_test()
        return 0
    if len(argv) != 3:
        print("usage: check-ledger-egress.py TRACE LEDGER_JSON", file=sys.stderr)
        return 2
    connects, binds = gather(argv[1])
    ledger = load_ledger(argv[2])
    undisclosed = missing(connects, binds, ledger)
    if undisclosed:
        print("ledger is missing a traced destination:", file=sys.stderr)
        for item in undisclosed:
            print(f"  {item}", file=sys.stderr)
        return 1
    print(
        f"check-ledger-egress: {len(connects)} connect and {len(binds)} bind destinations are in the ledger"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
