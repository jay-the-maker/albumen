#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail CI when a command or MCP tool has no network proof.

Reads the clap subcommand list from `albumen --help` (and each
subcommand's --help for its actions) and the albumen-mcp tools/list, then
checks every entry has a case in one of the proofs:

- scripts/zero-network-cases.sh (`zn_case <name>`) and the MCP cases in
  scripts/zero-network.sh (`mcp-<tool>`, plus `index`/`search` for the
  search tool): proven to make no connection.
- scripts/no-unexpected-egress.sh (`run_case <name>`): network-allowed
  modes, proven to reach only the address the invocation named.

A command `export` is covered by a case named `export` or `export-...`;
an action is `<command>-<action>` (`family-keygen`). Anything in neither
proof fails with a message naming it, so a new subcommand cannot land
unproven.

  check-network-coverage.py --albumen target/debug/albumen --mcp target/debug/albumen-mcp
  check-network-coverage.py --self-test
"""

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Known gaps, printed on every run and allowed to pass only while listed here.
# Each must get a proof before #5 is Met; remove the line when it lands.
PENDING = {
    "family-publish": "S3 upload; needs an allow-listed no-unexpected-egress case",
}

COMMANDS_RE = re.compile(r"^Commands:\s*$")


def commands_in(help_text):
    """Subcommand names under clap's `Commands:` heading, without `help`."""
    names = []
    inside = False
    for line in help_text.splitlines():
        if COMMANDS_RE.match(line):
            inside = True
            continue
        if inside:
            if not line.strip():
                break
            match = re.match(r"^\s{2}(\S+)", line)
            if match and match.group(1) != "help":
                names.append(match.group(1))
    return names


def cli_listing(run_help):
    """Coverage keys from a help runner: run_help([]) is `albumen --help`."""
    keys = []
    for command in commands_in(run_help([])):
        actions = commands_in(run_help([command]))
        if actions:
            keys.extend(f"{command}-{action}" for action in actions)
        else:
            keys.append(command)
    return keys


def binary_help(binary):
    def run(args):
        out = subprocess.run([binary, *args, "--help"], capture_output=True, text=True, check=True)
        return out.stdout
    return run


def frame(message):
    body = json.dumps(message).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body


def mcp_tools(binary):
    with tempfile.TemporaryDirectory() as library:
        stdin = frame({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}})
        stdin += frame({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        data = subprocess.run([binary, "--library", library], input=stdin, capture_output=True, check=True).stdout
    while data:
        header, _, rest = data.partition(b"\r\n\r\n")
        length = int(header.decode().split(":", 1)[1].strip())
        body, data = rest[:length], rest[length:]
        message = json.loads(body)
        if message.get("id") == 2:
            return [tool["name"] for tool in message["result"]["tools"]]
    raise SystemExit("check-network-coverage: albumen-mcp gave no tools/list answer")


def proven_cases(root):
    zero = set(re.findall(r"^\s*zn_case\s+([\w-]+)\s", (root / "scripts/zero-network-cases.sh").read_text(), re.M))
    main = (root / "scripts/zero-network.sh").read_text()
    zero |= set(re.findall(r"trace_cmd\s+(scan|index|search)\s", main))
    for tools in re.findall(r"for tool in ([\w ]+); do", main):
        zero |= {f"mcp-{tool}" for tool in tools.split()}
    if {"index", "search"} <= zero:
        zero.add("mcp-search")
    egress = set(re.findall(r"^run_case\s+([\w-]+)\s", (root / "scripts/no-unexpected-egress.sh").read_text(), re.M))
    return zero, egress


def covered(key, cases):
    return any(case == key or case.startswith(key + "-") for case in cases)


def check(keys, zero, egress, pending=PENDING):
    """Return (rows, missing). rows: (key, network, proof)."""
    rows, missing = [], []
    for key in keys:
        if covered(key, zero):
            rows.append((key, "No", "zero-network"))
        elif covered(key, egress):
            rows.append((key, "Only to the destination you name", "no-unexpected-egress"))
        elif key in pending:
            rows.append((key, "pending", pending[key]))
        else:
            missing.append(key)
    return rows, missing


FIXTURE_TOP = """Usage: albumen <COMMAND>

Commands:
  scan             Read a library and write a JSONL manifest outside it
  export           Copy the library and sidecars to a directory or s3://bucket/prefix
  frobnicate       A fake subcommand with no network proof
  help             Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
"""


def self_test():
    def fixture(top):
        return lambda args: top if not args else "Usage: albumen x\n\nOptions:\n  -h, --help\n"
    zero, egress = {"scan", "export"}, {"export-s3"}
    keys = cli_listing(fixture(FIXTURE_TOP))
    if keys != ["scan", "export", "frobnicate"]:
        raise SystemExit(f"check-network-coverage self-test: parsed {keys}")
    _, missing = check(keys, zero, egress, pending={})
    if missing != ["frobnicate"]:
        raise SystemExit(f"check-network-coverage self-test: frobnicate was not caught ({missing})")
    clean = FIXTURE_TOP.replace("  frobnicate       A fake subcommand with no network proof\n", "")
    _, missing = check(cli_listing(fixture(clean)), zero, egress, pending={})
    if missing:
        raise SystemExit(f"check-network-coverage self-test: clean listing failed ({missing})")
    print("check-network-coverage: self-test ok (frobnicate fails, the listing without it passes)")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--albumen", default=str(ROOT / "target/debug/albumen"))
    parser.add_argument("--mcp", default=str(ROOT / "target/debug/albumen-mcp"))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    keys = cli_listing(binary_help(args.albumen)) + [f"mcp-{tool}" for tool in mcp_tools(args.mcp)]
    zero, egress = proven_cases(ROOT)
    rows, missing = check(keys, zero, egress)
    for key, network, proof in rows:
        print(f"check-network-coverage: {key}: {network} ({proof})")
    for key in sorted(set(PENDING) - set(keys)):
        print(f"check-network-coverage: PENDING entry {key} is not a command any more; remove it", file=sys.stderr)
    if missing:
        for key in missing:
            print(f"check-network-coverage: {key} is in neither zero-network nor no-unexpected-egress", file=sys.stderr)
        sys.exit(1)
    print(f"check-network-coverage: {len(rows)} commands and tools, {len(missing)} unproven, {sum(r[1] == 'pending' for r in rows)} pending")


if __name__ == "__main__":
    main()
