#!/usr/bin/env python3
"""Check server.json against the official MCP registry schema and this repo.

Pass --tag v0.1.1 on a release tag. The tag, server.json, the OCI image tag,
and mcp/Cargo.toml must name the same version. The description is the tagline
and nothing else. The default mount of /library must be read-only.
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


LIBRARY_DST = "/library"
DESTINATION_DST = "/destination"


def fail(message: str) -> None:
    print(f"mcp release check failed: {message}", file=sys.stderr)
    sys.exit(1)


def load_server() -> dict:
    return json.loads((ROOT / "server.json").read_text())


def parse_mount(value: str) -> tuple[dict[str, str], set[str]]:
    options: dict[str, str] = {}
    flags: set[str] = set()
    for part in value.split(","):
        if not part:
            continue
        if "=" in part:
            key, val = part.split("=", 1)
            options[key.strip()] = val
        else:
            flags.add(part.strip())
    return options, flags


def mount_destination(options: dict[str, str]) -> str:
    raw = options.get("dst", options.get("destination", options.get("target", "")))
    if len(raw) > 1:
        raw = raw.rstrip("/")
    return raw


def mount_source(options: dict[str, str]) -> str:
    return options.get("src", options.get("source", ""))


def is_readonly_mount(flags: set[str]) -> bool:
    return "readonly" in flags or "ro" in flags


def runtime_mounts(server: dict) -> list[dict]:
    packages = server.get("packages") or []
    if len(packages) != 1 or not isinstance(packages[0], dict):
        return []
    arguments = packages[0].get("runtimeArguments") or []
    return [arg for arg in arguments if isinstance(arg, dict) and arg.get("name") == "--mount"]


def mount_errors(server: dict) -> list[str]:
    errors: list[str] = []
    library = []
    destinations = []
    for arg in runtime_mounts(server):
        options, flags = parse_mount(arg.get("value") or "")
        dst = mount_destination(options)
        if dst == LIBRARY_DST:
            library.append((arg, options, flags))
        elif dst == DESTINATION_DST:
            destinations.append((arg, options, flags))
    if len(library) != 1:
        errors.append(f"expected one mount of {LIBRARY_DST}, found {len(library)}")
    else:
        arg, options, flags = library[0]
        if arg.get("isRequired") is not True:
            errors.append("the /library mount must be required")
        if options.get("type") != "bind":
            errors.append("the /library mount must be a bind mount")
        if mount_source(options) != "{library_path}":
            errors.append("the /library mount must use {library_path}")
        if not is_readonly_mount(flags):
            errors.append("the default mount of /library is not read-only")
        library_var = (arg.get("variables") or {}).get("library_path") or {}
        if library_var.get("format") != "filepath" or library_var.get("isRequired") is not True:
            errors.append("library_path must be a required filepath")
    if len(destinations) != 1:
        errors.append(
            f"expected one optional mount of {DESTINATION_DST}, found {len(destinations)}"
        )
    else:
        arg, options, flags = destinations[0]
        if arg.get("isRequired") is not False:
            errors.append("the /destination mount must be optional")
        if options.get("type") != "bind":
            errors.append("the /destination mount must be a bind mount")
        if mount_source(options) != "{destination_path}":
            errors.append("the /destination mount must use {destination_path}")
        if mount_destination(options) == LIBRARY_DST or mount_source(options) == "{library_path}":
            errors.append("the writable mount must not be the library of originals")
        if is_readonly_mount(flags):
            errors.append("the /destination mount must stay writable")
        dest_var = (arg.get("variables") or {}).get("destination_path") or {}
        if dest_var.get("format") != "filepath" or dest_var.get("isRequired") is not True:
            errors.append("destination_path must be a required filepath")
    return errors


def library_mount_value(server: dict) -> str:
    errors = mount_errors(server)
    if errors:
        fail("; ".join(errors))
    for arg in runtime_mounts(server):
        options, _flags = parse_mount(arg.get("value") or "")
        if mount_destination(options) == LIBRARY_DST:
            return arg["value"]
    fail("missing /library mount")
    return ""


def self_test() -> int:
    server = load_server()
    errors = mount_errors(server)
    if errors:
        fail("server.json mount check: " + "; ".join(errors))

    writable = json.loads(json.dumps(server))
    for arg in runtime_mounts(writable):
        options, _flags = parse_mount(arg.get("value") or "")
        if mount_destination(options) == LIBRARY_DST:
            arg["value"] = str(arg["value"]).replace(",readonly", "").replace(",ro", "")
    writable_errors = mount_errors(writable)
    if not any("not read-only" in error for error in writable_errors):
        fail("a writable /library mount was accepted")

    aliased = json.loads(json.dumps(server))
    for arg in runtime_mounts(aliased):
        options, _flags = parse_mount(arg.get("value") or "")
        if mount_destination(options) == DESTINATION_DST:
            arg["value"] = str(arg["value"]).replace("dst=/destination", "dst=/library")
    if not mount_errors(aliased):
        fail("a second mount of /library was accepted")

    print("mcp mount self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", help="Git tag, for example v0.1.1")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Check that a writable /library mount is rejected",
    )
    parser.add_argument(
        "--print-library-mount",
        action="store_true",
        help="Print the default /library mount value",
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    server = load_server()
    if args.print_library_mount:
        print(library_mount_value(server))
        return 0

    try:
        import jsonschema
    except ImportError:
        fail("jsonschema is not installed")
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
    mount_problems = mount_errors(server)
    if mount_problems:
        fail("; ".join(mount_problems))

    if args.tag is not None:
        if not args.tag.startswith("v") or args.tag[1:] != version:
            fail(f"tag {args.tag} does not match version {version}")

    print(f"mcp release check passed ({version})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
