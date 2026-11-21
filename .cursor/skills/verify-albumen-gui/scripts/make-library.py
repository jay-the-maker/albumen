#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build a synthetic photo library for driving `albumen gui`.

    make-library.py OUT [--odd] [--scale N]

Default: 4 event folders, 26 JPEGs with EXIF capture dates, two cameras and
GPS on the "Harbour" event (written with the pinned ExifTool on PATH), plus
one PNG at the top level and one HEIC-named file that the grid counts but
does not preview. --odd adds a symlink and a control-character file name
(the GUI must skip and count them). --scale N writes N small JPEGs across
N/250 folders instead, for grid-performance checks (no EXIF, fast).

Needs Pillow (python3 -m pip install pillow) and, unless --scale, exiftool.
OUT must not exist. Nothing outside OUT is written.
"""
from __future__ import annotations

import argparse
import io
import os
import shutil
import subprocess
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw
except ImportError:
    sys.exit("make-library.py needs Pillow: python3 -m pip install pillow")

EVENTS = [
    # folder, date, camera (make, model), count, gps (lat, lon) or None
    ("2015 06 Family Reunion", "2015:06:20", ("Canon", "EOS 5D Mark III"), 8, None),
    ("2015 07 Harbour Walk", "2015:07:04", ("Canon", "EOS 5D Mark III"), 6, (47.6062, -122.3321)),
    ("2015 08 Garden", "2015:08:15", ("FUJIFILM", "X100T"), 7, None),
    ("2015 09 First Day of School", "2015:09:01", ("FUJIFILM", "X100T"), 5, (-33.8688, 151.2093)),
]

PALETTE = [(196, 92, 60), (60, 120, 180), (90, 160, 90), (200, 170, 60), (140, 90, 170), (60, 160, 160)]


def picture(w: int, h: int, seed: int, label: str) -> Image.Image:
    a = PALETTE[seed % len(PALETTE)]
    b = PALETTE[(seed * 7 + 3) % len(PALETTE)]
    strip = Image.new("RGB", (1, 64))
    for y in range(64):
        t = y / 63
        strip.putpixel((0, y), tuple(int(a[i] * (1 - t) + b[i] * t) for i in range(3)))
    img = strip.resize((w, h), Image.BILINEAR)
    d = ImageDraw.Draw(img)
    r = min(w, h) // 4
    cx, cy = (seed * 97) % (w - 2 * r) + r, (seed * 53) % (h - 2 * r) + r
    d.ellipse((cx - r, cy - r, cx + r, cy + r), fill=(250, 245, 230))
    d.rectangle((0, h - 60, w, h), fill=(20, 22, 26))
    d.text((16, h - 46), label, fill=(240, 240, 240))
    return img


def jpeg(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=82)
    return buf.getvalue()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out", type=Path)
    ap.add_argument("--odd", action="store_true")
    ap.add_argument("--scale", type=int, default=0)
    a = ap.parse_args()
    if a.out.exists():
        sys.exit(f"{a.out} already exists; pick a new folder")
    a.out.mkdir(parents=True)
    if a.scale:
        per = 250
        tiny = [jpeg(picture(160, 120, s, f"#{s}")) for s in range(12)]
        for n in range(a.scale):
            folder = a.out / f"{2000 + n // (per * 12)} {n // per:04d} Batch"
            folder.mkdir(exist_ok=True)
            (folder / f"IMG_{n:06d}.jpg").write_bytes(tiny[n % len(tiny)])
        print(f"{a.scale} photos in {(a.scale + per - 1) // per} folders under {a.out}")
        return 0
    exiftool = shutil.which("exiftool")
    if not exiftool:
        sys.exit("exiftool is not on PATH (export PATH=\"$(scripts/install-exiftool.sh):$PATH\")")
    n = 0
    for folder, date, (make, model), count, gps in EVENTS:
        d = a.out / folder
        d.mkdir()
        for i in range(count):
            n += 1
            p = d / f"IMG_{n:04d}.jpg"
            w, h = (1600, 1200) if i % 3 else (1200, 1600)
            p.write_bytes(jpeg(picture(w, h, n, f"{folder}  #{i + 1}")))
            args = [
                exiftool, "-q", "-overwrite_original",
                f"-DateTimeOriginal={date} {10 + i % 8:02d}:{(i * 7) % 60:02d}:00",
                f"-Make={make}", f"-Model={model}",
            ]
            if gps:
                lat, lon = gps
                args += [
                    f"-GPSLatitude={abs(lat)}", f"-GPSLatitudeRef={'N' if lat >= 0 else 'S'}",
                    f"-GPSLongitude={abs(lon)}", f"-GPSLongitudeRef={'E' if lon >= 0 else 'W'}",
                ]
            subprocess.run(args + [str(p)], check=True)
    picture(800, 600, 99, "loose photo").save(a.out / "loose.png")
    (a.out / "IMG_9000.HEIC").write_bytes(b"not a real heic; counted, not previewed")
    if a.odd:
        os.symlink(a.out / "2015 08 Garden" / "IMG_0015.jpg", a.out / "2015 08 Garden" / "shortcut.jpg")
        (a.out / "2015 08 Garden" / "bell\x07name.jpg").write_bytes(jpeg(picture(400, 300, 7, "odd")))
    print(f"{n} JPEGs in {len(EVENTS)} events, 1 PNG, 1 HEIC{', odd entries' if a.odd else ''} under {a.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
