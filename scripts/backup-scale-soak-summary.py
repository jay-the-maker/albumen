#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Add shard result lines and fail unless every shard reached 99.9% with no silent miss."""

import os
import sys
from pathlib import Path


def parse(line: str) -> dict[str, str]:
    out = {}
    for token in line.split():
        if "=" in token:
            key, value = token.split("=", 1)
            out[key] = value
    return out


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "build/backup-scale-soak")
    smoke = os.environ.get("ALBUMEN_SOAK_SMOKE", "0") == "1"
    expected_items = int(os.environ.get("ALBUMEN_SOAK_ITEMS", "8" if smoke else "10000"))
    expected_shards = int(os.environ.get("ALBUMEN_SOAK_SHARDS", "1" if smoke else "10"))
    files = sorted(root.rglob("*-result.txt"))
    if not files:
        print(f"no scale soak results under {root}", file=sys.stderr)
        return 1
    rows = []
    problems = []
    for path in files:
        text = path.read_text(encoding="utf-8").strip().splitlines()
        if not text:
            problems.append(f"{path.name} is empty")
            continue
        row = parse(text[0])
        row["_file"] = path.name
        rows.append(row)
        try:
            items = int(row["items"])
            verified = int(row["verified"])
            silent = int(row["silent_failures"])
            time_to = int(row["time_to_99_9_ms"])
        except (KeyError, ValueError) as exc:
            problems.append(f"{path.name} is missing a field ({exc})")
            continue
        if silent != 0:
            problems.append(f"{path.name} silent_failures={silent}")
        if items <= 0 or verified * 1000 < items * 999:
            problems.append(f"{path.name} verified {verified} of {items}")
        if time_to < 0:
            problems.append(f"{path.name} did not reach 99.9%")
    by_platform: dict[str, list[dict[str, str]]] = {}
    for row in rows:
        by_platform.setdefault(row.get("platform", "?"), []).append(row)
    for platform in ("android", "ios"):
        got = by_platform.get(platform, [])
        if len(got) != expected_shards:
            problems.append(f"{platform} has {len(got)} shards, expected {expected_shards}")
        if got and all("items" in row for row in got):
            total = sum(int(row["items"]) for row in got)
            if total != expected_items:
                problems.append(f"{platform} items {total}, expected {expected_items}")
    lines = [
        "# Backup scale soak",
        "",
        "Synthetic items only. Shards run in parallel, so each `time_to_99_9_ms` is that shard's clock. "
        "The slowest shard is the time until every shard has crossed 99.9% of its own items. "
        "A single runner does not execute all 10,000 items.",
        "",
        "| platform | shard | items | verified % | elapsed ms | time to 99.9% ms | silent |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for row in rows:
        lines.append(
            "| {platform} | {shard} | {items} | {verified_percent} | {elapsed_ms} | {time_to_99_9_ms} | {silent_failures} |".format(
                **{key: row.get(key, "") for key in (
                    "platform", "shard", "items", "verified_percent", "elapsed_ms", "time_to_99_9_ms", "silent_failures"
                )}
            )
        )
    if rows:
        slowest = max(int(row.get("time_to_99_9_ms", "0")) for row in rows)
        verified = sum(int(row["verified"]) for row in rows if "verified" in row)
        items = sum(int(row["items"]) for row in rows if "items" in row)
        percent = (verified * 1000 // items) / 10 if items else 0
        lines += [
            "",
            f"Overall verified {verified} of {items} ({percent}%). Slowest shard time to 99.9%: {slowest} ms.",
        ]
    summary = "\n".join(lines) + "\n"
    print(summary)
    step = os.environ.get("GITHUB_STEP_SUMMARY")
    if step:
        with open(step, "a", encoding="utf-8") as handle:
            handle.write(summary)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
