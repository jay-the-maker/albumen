#!/usr/bin/env python3
"""Check server.json against the official MCP registry schema and this repo.

Pass --tag v0.1.0 on a release tag. The tag, server.json, the OCI image tag,
and mcp/Cargo.toml must name the same version. The description is the tagline
and nothing else.
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
import urllib.request
from pathlib import Path

SCHEMA_URL = "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json"
TAGLINE = (
    "Albumen: the open-source alternative to iCloud Photos, Google Photos and Frame.io."
)
NAME = "io.github.jay-the-maker/albumen"
IMAGE = "ghcr.io/jay-the-maker/albumen-mcp"
ROOT = Path(__file__).resolve().parent.parent


def fail(message: str) -> None:
    print(f"mcp release check failed: {message}", file=sys.stderr)
    sys.exit(1)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", help="Git tag, for example v0.1.0")
    args = parser.parse_args()

    try:
        import jsonschema
    except ImportError:
        fail("jsonschema is not installed")

    server_path = ROOT / "server.json"
    server = json.loads(server_path.read_text())
    with urllib.request.urlopen(SCHEMA_URL, timeout=60) as response:
        schema = json.load(response)
    if schema.get("$id") != SCHEMA_URL:
        fail(f"schema $id is {schema.get('$id')}, expected {SCHEMA_URL}")
    jsonschema.validate(server, schema)

    with (ROOT / "mcp" / "Cargo.toml").open("rb") as cargo_file:
        cargo_version = tomllib.load(cargo_file)["package"]["version"]

    if server.get("name") != NAME:
        fail(f"name is {server.get('name')}")
    if server.get("description") != TAGLINE:
        fail("description is not the tagline")
    if len(server["description"]) > 100:
        fail("description is longer than 100 characters")
    version = server.get("version")
    if version != cargo_version:
        fail(f"server.json version {version} != mcp/Cargo.toml {cargo_version}")

    packages = server.get("packages") or []
    if len(packages) != 1:
        fail("expected one package")
    package = packages[0]
    if package.get("registryType") != "oci":
        fail("package is not oci")
    if package.get("identifier") != f"{IMAGE}:{version}":
        fail(f"identifier is {package.get('identifier')}")
    if "version" in package:
        fail("OCI packages must not set version; it belongs in the identifier tag")
    transport = package.get("transport") or {}
    if transport.get("type") != "stdio":
        fail("transport is not stdio")
    if package.get("runtimeHint") != "docker":
        fail("runtimeHint is not docker")

    if args.tag is not None:
        if not args.tag.startswith("v") or args.tag[1:] != version:
            fail(f"tag {args.tag} does not match version {version}")

    print(f"mcp release check passed ({version})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
