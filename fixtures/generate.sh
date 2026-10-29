#!/usr/bin/env bash
# Generate the CC0 fixture corpus. The committed bytes are canonical.
# Re-run only when intentionally refreshing the corpus, then update
# fixtures/README.md with the tool versions this script prints.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required tool: $1" >&2
    exit 1
  }
}
need convert
need exiftool
need ffmpeg
need heif-enc
need python3
need sha256sum

rm -rf jpeg-gps heic raw-jpeg xmp live-photo video motion-photo aae ultra-hdr
mkdir -p jpeg-gps heic raw-jpeg xmp live-photo video motion-photo aae ultra-hdr

echo "tool versions:"
exiftool -ver
heif-enc --version | head -n 1
ffmpeg -version | head -n 1
convert -version | head -n 2

write_jpeg() {
  local dest="$1" color="$2"
  convert -size 640x480 "xc:${color}" -strip -quality 92 "JPEG:${dest}"
}

# JPEG with embedded EXIF, GPS, IPTC, and XMP.
write_jpeg jpeg-gps/IMG_0001.jpg '#4A6FA5'
exiftool -overwrite_original -n \
  -Make=Albumen \
  -Model="Fixture Camera" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -ImageDescription="Synthetic CC0 JPEG with GPS, IPTC, and XMP. Not a photograph." \
  -DateTimeOriginal="2012:06:15 10:30:00" \
  -CreateDate="2012:06:15 10:30:00" \
  -OffsetTimeOriginal="-07:00" \
  -GPSLatitude=37.8199 \
  -GPSLatitudeRef=N \
  -GPSLongitude=-122.4783 \
  -GPSLongitudeRef=W \
  -GPSAltitude=16 \
  -GPSAltitudeRef=0 \
  -ExposureTime=0.008 \
  -FNumber=4 \
  -ISO=200 \
  -FocalLength=35 \
  -IPTC:ObjectName="Albumen fixture JPEG" \
  -IPTC:Caption-Abstract="Synthetic CC0 JPEG with GPS, IPTC, and XMP. Not a photograph." \
  -IPTC:Keywords=albumen \
  -IPTC:Keywords=cc0 \
  -IPTC:By-line="Albumen Project" \
  -IPTC:CopyrightNotice="CC0-1.0" \
  -XMP-dc:Title="Albumen fixture JPEG" \
  -XMP-dc:Rights="CC0 1.0 Universal" \
  -XMP-xmp:CreatorTool="albumen fixtures/generate.sh" \
  jpeg-gps/IMG_0001.jpg >/dev/null

# HEIC still.
convert -size 320x240 'xc:#C4A574' -strip PNG:heic/src.png
heif-enc -q 70 -o heic/IMG_0002.heic heic/src.png >/dev/null
rm -f heic/src.png
exiftool -overwrite_original \
  -Make=Albumen \
  -Model="Fixture HEIC" \
  -DateTimeOriginal="2018:04:01 12:00:00" \
  -CreateDate="2018:04:01 12:00:00" \
  -GPSLatitude="48 deg 51' 24.00\" N" \
  -GPSLongitude="2 deg 21' 8.00\" E" \
  -ImageDescription="Synthetic CC0 HEIC still. Not a photograph." \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  heic/IMG_0002.heic >/dev/null

# RAW+JPEG pair. The RAW is a synthetic DNG (TIFF container plus DNG tags).
write_jpeg raw-jpeg/IMG_0003.jpg '#6B4C7A'
convert -size 64x48 'gradient:#222222-#eeeeee' -depth 16 -compress none -strip raw-jpeg/IMG_0003.tif
exiftool -overwrite_original -n \
  -DNGVersion="1 4 0 0" \
  -DNGBackwardVersion="1 1 0 0" \
  -UniqueCameraModel="Albumen Fixture Camera" \
  -Make=Albumen \
  -Model="Synthetic DNG" \
  -DateTimeOriginal="2015:09:03 18:45:12" \
  -CreateDate="2015:09:03 18:45:12" \
  -ImageUniqueID="0123456789ABCDEF0123456789ABCDEF" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -ImageDescription="Synthetic CC0 DNG paired with IMG_0003.jpg. Not a camera capture." \
  raw-jpeg/IMG_0003.tif >/dev/null
mv raw-jpeg/IMG_0003.tif raw-jpeg/IMG_0003.dng
exiftool -overwrite_original -n \
  -Make=Albumen \
  -Model="Fixture Camera" \
  -DateTimeOriginal="2015:09:03 18:45:12" \
  -CreateDate="2015:09:03 18:45:12" \
  -ImageUniqueID="0123456789ABCDEF0123456789ABCDEF" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -ImageDescription="Synthetic CC0 JPEG paired with IMG_0003.dng. Not a photograph." \
  -GPSLatitude=36.1699 \
  -GPSLatitudeRef=N \
  -GPSLongitude=-115.1398 \
  -GPSLongitudeRef=W \
  raw-jpeg/IMG_0003.jpg >/dev/null

# JPEG plus an XMP sidecar.
write_jpeg xmp/IMG_0004.jpg '#2F6F4E'
exiftool -overwrite_original \
  -Make=Albumen \
  -Model="Fixture Camera" \
  -DateTimeOriginal="2016:03:21 09:15:00" \
  -CreateDate="2016:03:21 09:15:00" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -ImageDescription="Synthetic CC0 JPEG that ships with an XMP sidecar." \
  xmp/IMG_0004.jpg >/dev/null
cat > xmp/IMG_0004.xmp <<'EOF'
<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Albumen Fixture Generator">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Albumen CC0 sidecar fixture. Not a photograph.</rdf:li>
    </rdf:Alt>
   </dc:description>
   <dc:rights>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">CC0 1.0 Universal</rdf:li>
    </rdf:Alt>
   </dc:rights>
   <xmp:Rating>5</xmp:Rating>
   <xmp:Label>albumen-fixture</xmp:Label>
   <xmp:CreatorTool>albumen fixtures/generate.sh</xmp:CreatorTool>
   <photoshop:DateCreated>2016-03-21T09:15:00</photoshop:DateCreated>
   <exif:GPSLatitude>48,51.400000N</exif:GPSLatitude>
   <exif:GPSLongitude>2,21.133333E</exif:GPSLongitude>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
EOF

# Live Photo pair: HEIC still + MOV with the same Apple content identifier.
convert -size 320x240 'xc:#8C3A3A' -strip PNG:live-photo/src.png
heif-enc -q 70 -o live-photo/IMG_0005.heic live-photo/src.png >/dev/null
rm -f live-photo/src.png
CONTENT_ID="B4E3C1A0-7F2D-4C8A-9E11-0A1B2C3D4E5F"
# -Apple:ContentIdentifier does not create a maker-note directory. The still
# gets a generated block (header Apple iOS, IFD tag 0x0011 at byte 14) and the
# same UUID in XMP. The MOV carries Keys:ContentIdentifier.
exiftool -overwrite_original \
  -Make=Albumen \
  -Model="Fixture Live Photo" \
  -DateTimeOriginal="2020:01:02 03:04:05" \
  -CreateDate="2020:01:02 03:04:05" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -ImageDescription="Synthetic CC0 Live Photo still. Pair with IMG_0005.mov." \
  -XMP-dc:Identifier="$CONTENT_ID" \
  live-photo/IMG_0005.heic >/dev/null
python3 - "$CONTENT_ID" live-photo/apple-makernote.bin <<'PY'
import struct, sys
text = sys.argv[1].encode("ascii") + b"\x00"
entry = struct.pack(">HHII", 0x0011, 2, len(text), 32)
ifd = struct.pack(">H", 1) + entry + struct.pack(">I", 0)
open(sys.argv[2], "wb").write(b"Apple iOS\x00\x00\x01MM" + ifd + text)
PY
exiftool -overwrite_original -m \
  -EXIF:MakerNoteApple"<=-" \
  live-photo/IMG_0005.heic <live-photo/apple-makernote.bin >/dev/null
rm -f live-photo/apple-makernote.bin

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "color=c=0x8C3A3A:s=320x240:d=1,format=yuv420p" \
  -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=1" \
  -c:v libx264 -pix_fmt yuv420p -preset ultrafast -crf 32 \
  -c:a aac -shortest \
  -metadata creation_time="2020-01-02T03:04:05Z" \
  -metadata title="Albumen CC0 live photo" \
  -metadata artist="Albumen Project" \
  -metadata copyright="CC0-1.0" \
  live-photo/IMG_0005.mov
exiftool -overwrite_original \
  -Keys:ContentIdentifier="$CONTENT_ID" \
  -Keys:Title="Albumen CC0 live photo" \
  live-photo/IMG_0005.mov >/dev/null

# Standalone video.
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "color=c=0x2E8B57:s=320x240:d=1,format=yuv420p" \
  -c:v libx264 -pix_fmt yuv420p -preset ultrafast -crf 32 \
  -metadata creation_time="2019-08-01T12:00:00Z" \
  -metadata title="Albumen CC0 video" \
  -metadata artist="Albumen Project" \
  -metadata copyright="CC0-1.0" \
  -metadata comment="Synthetic CC0 video. Not a recording of a real scene." \
  video/IMG_0006.mp4

# Android Motion Photo: JPEG with an MP4 appended after EOI. XMP carries
# MicroVideoOffset and the Container MotionPhoto item length.
convert -size 320x240 'xc:#335577' -strip -quality 90 motion-photo/base.jpg
exiftool -overwrite_original \
  -Make=Albumen \
  -Model="Fixture Motion Photo" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -DateTimeOriginal="2021:05:04 16:20:00" \
  -CreateDate="2021:05:04 16:20:00" \
  motion-photo/base.jpg >/dev/null
ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "color=c=0x335577:s=160x120:d=1,format=yuv420p" \
  -c:v libx264 -pix_fmt yuv420p -preset ultrafast -crf 35 -an \
  motion-photo/clip.mp4

# Ultra HDR primary and a tiny grayscale gain map. assemble.py wraps them.
convert -size 640x480 'xc:#E8D5A3' -strip -quality 90 ultra-hdr/base.jpg
convert -size 32x24 'gradient:#202020-#e0e0e0' -colorspace gray -strip -quality 85 ultra-hdr/gain.jpg

python3 "$ROOT/assemble.py" \
  motion-photo/base.jpg motion-photo/clip.mp4 motion-photo/IMG_0007.jpg \
  ultra-hdr/base.jpg ultra-hdr/gain.jpg ultra-hdr/IMG_0009.jpg
rm -f motion-photo/base.jpg motion-photo/clip.mp4 ultra-hdr/base.jpg ultra-hdr/gain.jpg

# Apple Photos edit sidecar. Non-destructive adjustments live beside the still.
convert -size 320x240 'xc:#D4A017' -strip -quality 90 aae/IMG_0008.jpg
exiftool -overwrite_original \
  -Make=Albumen \
  -Model="Fixture Camera" \
  -Artist="Albumen Project" \
  -Copyright="CC0-1.0" \
  -DateTimeOriginal="2017:11:09 08:40:00" \
  -CreateDate="2017:11:09 08:40:00" \
  -ImageDescription="Synthetic CC0 JPEG paired with an Apple .aae edit sidecar." \
  aae/IMG_0008.jpg >/dev/null
cat > aae/IMG_0008.aae <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
 <key>adjustmentFormatIdentifier</key>
 <string>com.apple.photo</string>
 <key>adjustmentFormatVersion</key>
 <integer>1</integer>
 <key>adjustmentData</key>
 <data>
  AAE=
 </data>
 <key>adjustments</key>
 <array>
  <dict>
   <key>identifier</key>
   <string>com.apple.photo.adjustment.contrast</string>
   <key>formatIdentifier</key>
   <string>com.apple.photo</string>
   <key>formatVersion</key>
   <integer>1</integer>
   <key>settings</key>
   <dict>
    <key>inputContrast</key>
    <real>0.35</real>
    <key>inputBrightness</key>
    <real>0.10</real>
   </dict>
  </dict>
 </array>
</dict>
</plist>
EOF

# Drop generator leftovers exiftool may leave behind.
find . -name '*_original' -delete

# IPTC Core is written onto copies of the JPEG and HEIC this script just
# produced. Another ExifTool version will not match the pinned SHA-256.
bash "$ROOT/../scripts/write-iptc-core.sh" "$ROOT/iptc"

# Sidecar naming edge cases (issue #42), also copies under ExifTool 13.59.
rm -rf "$ROOT/sidecar-names"
bash "$ROOT/../scripts/write-sidecar-names.sh" "$ROOT/sidecar-names"

(
  cd "$ROOT"
  find jpeg-gps heic raw-jpeg xmp live-photo video motion-photo aae ultra-hdr iptc sidecar-names -type f ! -name 'src.png' -print0 \
    | sort -z \
    | xargs -0 sha256sum
) > SHA256SUMS

echo "wrote fixtures:"
cat SHA256SUMS
