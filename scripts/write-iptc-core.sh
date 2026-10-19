#!/usr/bin/env bash
# Write IPTC Core onto copies of the committed CC0 JPEG and HEIC.
# Image-ExifTool 13.59 produces the same bytes on every run. The SHA-256 pins
# are in tests/fixtures/LICENSES.md and fixtures/SHA256SUMS.
set -euo pipefail

dest="${1:?usage: write-iptc-core.sh DIR}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"

if ! command -v exiftool >/dev/null 2>&1; then
  echo "exiftool is required to write IPTC Core" >&2
  exit 1
fi

win_native=0
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    win_native=1
    export MSYS2_ARG_CONV_EXCL='*'
    export MSYS_NO_PATHCONV=1
    ;;
esac

native() {
  if [[ "$win_native" -eq 1 ]]; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

cp "$root/fixtures/jpeg-gps/IMG_0001.jpg" "$dest/IMG_0001.jpg"
cp "$root/fixtures/heic/IMG_0002.heic" "$dest/IMG_0002.heic"

# Dates are fixed so the XMP packet does not pick up the clock.
exiftool -overwrite_original -m \
  -EXIF:ModifyDate="2012:06:15 10:30:00" \
  -XMP-xmp:ModifyDate="2012:06:15T10:30:00" \
  -XMP-xmp:MetadataDate="2012:06:15T10:30:00" \
  -IPTC:By-line="Albumen Project" \
  -IPTC:Caption-Abstract="Synthetic CC0 fixture. Not a photograph." \
  -IPTC:Keywords=albumen \
  -IPTC:Keywords=cc0 \
  -IPTC:City="Fixture City" \
  -IPTC:Country-PrimaryLocationName="Fixture Country" \
  -IPTC:CopyrightNotice="CC0 1.0 Universal" \
  -XMP-dc:Creator="Albumen Project" \
  -XMP-dc:Description="Synthetic CC0 fixture. Not a photograph." \
  -XMP-dc:Subject=albumen \
  -XMP-dc:Subject=cc0 \
  -XMP-dc:Rights="CC0 1.0 Universal" \
  -XMP-photoshop:City="Fixture City" \
  -XMP-photoshop:Country="Fixture Country" \
  -XMP-iptcCore:CountryCode=XC \
  -XMP-iptcCore:Location="Synthetic location" \
  "$(native "$dest/IMG_0001.jpg")" >/dev/null

# HEIC has no IPTC IIM box. The same records are stored as EXIF IPTC-NAA,
# which ExifTool reads back in the IPTC group. The block is the JPEG's IIM,
# passed on stdin so a Windows path is not part of the write.
exiftool -iptc -b "$(native "$dest/IMG_0001.jpg")" >"$dest/iptc.bin"
exiftool -overwrite_original -m \
  -EXIF:IPTC-NAA"<=-" \
  -EXIF:ModifyDate="2012:06:15 10:30:00" \
  -XMP-xmp:ModifyDate="2012:06:15T10:30:00" \
  -XMP-xmp:MetadataDate="2012:06:15T10:30:00" \
  -XMP-dc:Creator="Albumen Project" \
  -XMP-dc:Description="Synthetic CC0 fixture. Not a photograph." \
  -XMP-dc:Subject=albumen \
  -XMP-dc:Subject=cc0 \
  -XMP-dc:Rights="CC0 1.0 Universal" \
  -XMP-photoshop:City="Fixture City" \
  -XMP-photoshop:Country="Fixture Country" \
  -XMP-iptcCore:CountryCode=XC \
  -XMP-iptcCore:Location="Synthetic location" \
  "$(native "$dest/IMG_0002.heic")" <"$dest/iptc.bin" >/dev/null
rm -f "$dest/iptc.bin"
