#!/usr/bin/env python3
"""Fail unless a pull request body checks the philosophy box and explains it."""

import os
import re
import sys

CHECKED_TEXT = "I have read DESIGN_PHILOSOPHY.md and this change upholds it"
PLACEHOLDER = (
    "Replace this sentence with how the change keeps originals bit-exact and metadata intact."
)
HEADING = "## How this upholds it"
MIN_EXPLANATION = 20


def is_checked(line: str) -> bool:
    stripped = line.strip()
    if not stripped.startswith("- [") or len(stripped) < 5 or stripped[4] != "]":
        return False
    if stripped[3] not in ("x", "X"):
        return False
    return stripped[5:].lstrip() == CHECKED_TEXT


def strip_comments(text: str) -> str:
    return re.sub(r"<!--.*?-->", "", text, flags=re.DOTALL)


def explanation(body: str) -> str | None:
    lines = body.splitlines()
    start = None
    for index, line in enumerate(lines):
        if line.strip() == HEADING:
            start = index + 1
            break
    if start is None:
        return None
    end = len(lines)
    for index in range(start, len(lines)):
        if lines[index].startswith("#"):
            end = index
            break
    return "\n".join(lines[start:end]).strip()


def problems(body: str) -> list[str]:
    visible = strip_comments(body or "")
    found: list[str] = []
    checked = any(is_checked(line) for line in visible.splitlines())
    if not checked:
        found.append("the philosophy checkbox is not checked")
    text = explanation(visible)
    if text is None:
        found.append("missing heading 'How this upholds it'")
        return found
    if any(line.strip() == PLACEHOLDER for line in text.splitlines()):
        found.append("the explanation is still the template placeholder")
    if len(text) < MIN_EXPLANATION:
        found.append(
            f"the explanation is {len(text)} characters; it needs at least {MIN_EXPLANATION}"
        )
    return found


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
        return self_test()
    body = os.environ.get("PR_BODY")
    if body is None:
        print("PR_BODY is not set", file=sys.stderr)
        return 1
    found = problems(body)
    if found:
        print("philosophy-ack failed:", file=sys.stderr)
        for item in found:
            print(f"- {item}", file=sys.stderr)
        return 1
    print("philosophy-ack passed")
    return 0


def self_test() -> int:
    good = (
        "- [x] I have read DESIGN_PHILOSOPHY.md and this change upholds it\n\n"
        "## How this upholds it\n\n"
        "This change keeps the original bytes and the embedded metadata intact.\n\n"
        "## Test plan\n\n"
        "- [x] checked\n"
    )
    cases = [
        ("checked explanation", good, []),
        (
            "markers around a valid body",
            "<!-- CURSOR_AGENT_PR_BODY_BEGIN -->\n" + good + "<!-- CURSOR_AGENT_PR_BODY_END -->\n",
            [],
        ),
        (
            "unchecked",
            good.replace("- [x]", "- [ ]", 1),
            ["the philosophy checkbox is not checked"],
        ),
        (
            "placeholder",
            good.replace(
                "This change keeps the original bytes and the embedded metadata intact.",
                PLACEHOLDER,
            ),
            ["the explanation is still the template placeholder"],
        ),
        (
            "too short",
            good.replace(
                "This change keeps the original bytes and the embedded metadata intact.",
                "Too short.",
            ),
            ["the explanation is 10 characters; it needs at least 20"],
        ),
        (
            "commented-out box",
            "<!-- - [x] I have read DESIGN_PHILOSOPHY.md and this change upholds it -->\n"
            "## How this upholds it\n\n"
            "This change keeps the original bytes and the embedded metadata intact.\n",
            ["the philosophy checkbox is not checked"],
        ),
        (
            "explanation only in a comment",
            "- [x] I have read DESIGN_PHILOSOPHY.md and this change upholds it\n\n"
            "## How this upholds it\n\n"
            f"{PLACEHOLDER}\n"
            "<!-- This change keeps the original bytes and the embedded metadata intact. -->\n",
            ["the explanation is still the template placeholder"],
        ),
        (
            "exactly twenty characters",
            good.replace(
                "This change keeps the original bytes and the embedded metadata intact.",
                "abcdefghijabcdefghij",
            ),
            [],
        ),
    ]
    failed = 0
    for name, body, expected in cases:
        got = problems(body)
        if got != expected:
            print(f"FAIL {name}: got {got!r}, expected {expected!r}", file=sys.stderr)
            failed = 1
        else:
            print(f"ok {name}")
    return failed


if __name__ == "__main__":
    sys.exit(main())
