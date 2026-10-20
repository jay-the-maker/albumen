#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fetch and verify the pinned public test corpora.

The media is never committed. Each manifest pins every file by size, SHA-1
and SHA-256. Any mismatch fails hard and is never retried: the fix is a
manifest PR, not a re-run.

  fetch-commons-corpus.py [--corpus NAME] fetch DIR    download missing files, then verify all
  fetch-commons-corpus.py [--corpus NAME] verify DIR   verify only (a cache hit runs this)
  fetch-commons-corpus.py [--corpus NAME] attribution  rewrite the attribution file and docs section
  fetch-commons-corpus.py [--corpus NAME] attribution --check   fail if either is stale

NAME is one of: commons (default, Wikimedia Commons), media (CC0 HEIC, MOV and
MP4 from the Internet Archive), raw (CC0 camera RAW from raw.pixls.us).

Only the CI corpus job calls this. Default scan, index and search never touch
the network.
"""
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPORA = {
    "commons": ("commons.manifest.json", "ATTRIBUTION.md", "commons-corpus.md",
                ("https://upload.wikimedia.org/",)),
    "media": ("media.manifest.json", "MEDIA_ATTRIBUTION.md", "media-corpus.md",
              ("https://archive.org/download/",)),
    "raw": ("raw.manifest.json", "RAW_ATTRIBUTION.md", "raw-corpus.md",
            ("https://raw.pixls.us/getfile.php/",)),
}
MANIFEST = ATTRIBUTION = DOC = None
URL_PREFIXES = ()
START = "<!-- attribution:start -->"
END = "<!-- attribution:end -->"
USER_AGENT = (
    "albumen-ci-corpus/1 (https://github.com/jay-the-maker/albumen; "
    "pinned test corpus fetch)"
)
HTTP_ATTEMPTS = 5


def load():
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if data.get("schema") != 1:
        sys.exit(f"commons-corpus bad-manifest schema={data.get('schema')!r}")
    names = set()
    for f in data["files"]:
        name = f["local_name"]
        if "/" in name or "\\" in name or name.startswith(".") or name in names:
            sys.exit(f"commons-corpus bad-manifest local_name={name!r}")
        names.add(name)
        if not f["url"].startswith(URL_PREFIXES):
            sys.exit(f"commons-corpus bad-manifest url={f['url']!r}")
    return data


def digests(path):
    sha1 = hashlib.sha1()
    sha256 = hashlib.sha256()
    size = 0
    with open(path, "rb") as fh:
        while True:
            chunk = fh.read(1 << 20)
            if not chunk:
                break
            sha1.update(chunk)
            sha256.update(chunk)
            size += len(chunk)
    return size, sha1.hexdigest(), sha256.hexdigest()


def check(entry, path):
    size, sha1, sha256 = digests(path)
    bad = []
    if size != entry["size"]:
        bad.append(f"expected_size={entry['size']} got={size}")
    if sha1 != entry["sha1"]:
        bad.append(f"expected_sha1={entry['sha1']} got={sha1}")
    if sha256 != entry["sha256"]:
        bad.append(f"expected_sha256={entry['sha256']} got={sha256}")
    return bad


def download(entry, dest):
    tmp = dest.with_name(dest.name + ".part")
    for attempt in range(1, HTTP_ATTEMPTS + 1):
        try:
            req = urllib.request.Request(entry["url"], headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(req, timeout=120) as resp, open(tmp, "wb") as out:
                while True:
                    chunk = resp.read(1 << 20)
                    if not chunk:
                        break
                    out.write(chunk)
            break
        except (urllib.error.URLError, TimeoutError, ConnectionError) as err:
            tmp.unlink(missing_ok=True)
            code = getattr(err, "code", None)
            transient = code is None or code == 429 or code >= 500
            if attempt == HTTP_ATTEMPTS or not transient:
                sys.exit(
                    f"commons-corpus fetch-failed name={entry['local_name']} "
                    f"url={entry['url']} error={err}"
                )
            print(f"commons-corpus http-retry name={entry['local_name']} attempt={attempt} error={err}")
            time.sleep(5 * attempt)
    bad = check(entry, tmp)
    if bad:
        tmp.unlink(missing_ok=True)
        sys.exit(f"commons-corpus mismatch name={entry['local_name']} {' '.join(bad)}")
    os.chmod(tmp, 0o444)
    os.replace(tmp, dest)


def verify_all(data, root):
    failed = False
    total = 0
    for entry in data["files"]:
        path = root / entry["local_name"]
        if not path.is_file():
            print(f"commons-corpus missing name={entry['local_name']}")
            failed = True
            continue
        bad = check(entry, path)
        if bad:
            print(f"commons-corpus mismatch name={entry['local_name']} {' '.join(bad)}")
            failed = True
            continue
        total += entry["size"]
    expected = {e["local_name"] for e in data["files"]}
    extra = sorted(p.name for p in root.iterdir() if p.name not in expected)
    if extra:
        print(f"commons-corpus unexpected-files {extra}")
        failed = True
    if failed:
        sys.exit(1)
    print(f"commons-corpus verified files={len(data['files'])} bytes={total}")


def attribution_text(data):
    lines = [
        f"# {data.get('heading', data['source'])} test corpus: attribution",
        "",
        "<!-- Generated by scripts/fetch-commons-corpus.py attribution. Do not edit. -->",
        "",
        "These files are **not** in this repository. CI downloads them at run time",
        f"from {data['source']}, verifies them against `{MANIFEST.name}`",
        "(size, SHA-1, SHA-256) and uses them unmodified, only to test that",
        "Albumen copies originals bit-exactly and keeps their metadata.",
        "No file was changed. Licenses belong to the authors listed below.",
        "",
        f"| # | File ({data.get('page_label', 'Commons page')}) | Author | License |",
        "|---|---|---|---|",
    ]
    for i, f in enumerate(data["files"], 1):
        lic = f["license"]
        if f.get("license_url"):
            lic = f"[{lic}]({f['license_url']})"
        title = f["title"].removeprefix("File:").replace("|", "\\|")
        lines.append(f"| {i} | [{title}]({f['page']}) | {f['author']} | {lic} |")
    lines += [
        "",
        "Attribution strings, as required by the CC BY and CC BY-SA licenses:",
        "",
    ]
    if not any(f["license"].startswith("CC BY") for f in data["files"]):
        lines.append("- None required: every file above is CC0 or public domain.")
    for f in data["files"]:
        if f["license"].startswith("CC BY"):
            title = f["title"].removeprefix("File:")
            lines.append(f"- \"{title}\" by {f['author']}, {f['license']} ({f['license_url']}), via {data['source']}: {f['page']}")
    lines.append("")
    return "\n".join(lines)


def main(argv):
    global MANIFEST, ATTRIBUTION, DOC, URL_PREFIXES
    name = "commons"
    if len(argv) >= 3 and argv[1] == "--corpus":
        name = argv[2]
        argv = [argv[0]] + argv[3:]
    if name not in CORPORA:
        sys.exit(f"unknown corpus {name!r}; expected one of {sorted(CORPORA)}")
    manifest, attribution, doc, URL_PREFIXES = CORPORA[name]
    MANIFEST = ROOT / "corpus" / manifest
    ATTRIBUTION = ROOT / "corpus" / attribution
    DOC = ROOT / "docs" / doc
    if len(argv) >= 2 and argv[1] == "attribution":
        text = attribution_text(load())
        doc = DOC.read_text(encoding="utf-8")
        head, rest = doc.split(START, 1)
        _, tail = rest.split(END, 1)
        body = text.split("\n", 4)[4]
        new_doc = f"{head}{START}\n{body}{END}{tail}"
        if "--check" in argv:
            if ATTRIBUTION.read_text(encoding="utf-8") != text or doc != new_doc:
                sys.exit(f"corpus/{ATTRIBUTION.name} or docs/{DOC.name} is stale: "
                         f"run scripts/fetch-commons-corpus.py --corpus {name} attribution")
            print("commons-corpus attribution up to date")
            return
        ATTRIBUTION.write_text(text, encoding="utf-8")
        DOC.write_text(new_doc, encoding="utf-8")
        return
    if len(argv) != 3 or argv[1] not in ("fetch", "verify"):
        sys.exit(__doc__)
    data = load()
    root = Path(argv[2])
    root.mkdir(parents=True, exist_ok=True)
    if argv[1] == "fetch":
        for entry in data["files"]:
            dest = root / entry["local_name"]
            if dest.is_file():
                continue
            print(f"commons-corpus fetch name={entry['local_name']} bytes={entry['size']}")
            download(entry, dest)
            time.sleep(1)
    verify_all(data, root)


if __name__ == "__main__":
    main(sys.argv)
