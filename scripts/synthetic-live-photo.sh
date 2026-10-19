#!/usr/bin/env bash
# Generate a synthetic CC0 Live Photo pair in the directory given as $1.
# The bytes are not a photograph. ffmpeg encodes the pixels. This script wraps
# the HEVC still in a single-item HEIC container because the ffmpeg build used
# in CI has libx265 and no HEIF muxer, and heif-enc is not installed.
# ExifTool then writes the shared UUID. The last line of stdout is the HEIC
# tag that stuck, one of:
#   Apple:ContentIdentifier
#   Keys:ContentIdentifier
#   XMP-dc:Identifier
set -euo pipefail

out="${1:?usage: synthetic-live-photo.sh DIR}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "ffmpeg is required to generate the synthetic Live Photo" >&2
  exit 1
fi
if ! command -v exiftool >/dev/null 2>&1; then
  echo "exiftool is required to write the Live Photo content identifier" >&2
  exit 1
fi
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  echo "python3 is required to wrap the HEVC still as HEIC" >&2
  exit 1
fi

CONTENT_ID="A1B2C3D4-E5F6-4071-8A9B-C0D1E2F30405"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "color=c=0x336699:s=64x64:d=1,format=yuv420p" \
  -frames:v 1 -c:v libx265 -pix_fmt yuv420p -tag:v hvc1 \
  -x265-params still-picture=1 \
  "$work/still.mp4" >/dev/null

"$PY" - "$work/still.mp4" "$work/IMG_LIVE.heic" <<'PY'
import struct, sys

def box(typ, payload):
    return struct.pack(">I", 8 + len(payload)) + typ + payload

def children(data, start, end):
    i = start
    while i + 8 <= end:
        size = struct.unpack(">I", data[i:i + 4])[0]
        typ = data[i + 4:i + 8]
        header = 8
        if size == 1:
            size = struct.unpack(">Q", data[i + 8:i + 16])[0]
            header = 16
        elif size == 0:
            size = end - i
        if size < header or i + size > end:
            raise SystemExit(f"bad box {typ!r} at {i}")
        yield i, size, header, typ
        i += size

def find(data, typ, start=0, end=None):
    if end is None:
        end = len(data)
    for i, size, header, found in children(data, start, end):
        if found == typ:
            return i, size, header
        if found in (b"moov", b"trak", b"mdia", b"minf", b"stbl", b"dinf"):
            hit = find(data, typ, i + header, i + size)
            if hit:
                return hit
    return None

mp4 = open(sys.argv[1], "rb").read()
stsd = find(mp4, b"stsd")
if not stsd:
    raise SystemExit("mp4 has no stsd")
_, stsd_size, stsd_header = stsd
sample_entry = find(mp4, b"hvc1", stsd[0] + stsd_header + 8, stsd[0] + stsd_size)
if not sample_entry:
    raise SystemExit("mp4 has no hvc1 sample")
entry_at, entry_size, entry_header = sample_entry
entry = mp4[entry_at:entry_at + entry_size]
# VisualSampleEntry is 78 bytes before the child boxes.
width, height = struct.unpack(">HH", entry[entry_header + 24:entry_header + 28])
hvcc_at = entry.find(b"hvcC")
if hvcc_at < 4:
    raise SystemExit("mp4 has no hvcC")
hvcc_size = struct.unpack(">I", entry[hvcc_at - 4:hvcc_at])[0]
hvcc = entry[hvcc_at - 4:hvcc_at - 4 + hvcc_size]
stsz = find(mp4, b"stsz")
stco = find(mp4, b"stco")
if not stsz or not stco:
    raise SystemExit("mp4 has no sample location")
sample_size = struct.unpack(">I", mp4[stsz[0] + 12:stsz[0] + 16])[0]
sample_count = struct.unpack(">I", mp4[stsz[0] + 16:stsz[0] + 20])[0]
offset = struct.unpack(">I", mp4[stco[0] + 16:stco[0] + 20])[0]
if sample_count != 1 or sample_size < 5:
    raise SystemExit(f"expected one HEVC sample, got count={sample_count} size={sample_size}")
sample = mp4[offset:offset + sample_size]
if len(sample) != sample_size:
    raise SystemExit("HEVC sample is truncated")

ispe = box(b"ispe", struct.pack(">III", 0, width, height))
colr = box(b"colr", bytes.fromhex("6e636c780001000d000680"))
pixi = box(b"pixi", bytes.fromhex("0000000003080808"))
ipco = box(b"ipco", hvcc + ispe + colr + pixi)
ipma = box(b"ipma", bytes.fromhex("000000000000000100010481020304"))
iprp = box(b"iprp", ipco + ipma)
hdlr = box(b"hdlr", bytes.fromhex("00000000000000007069637400000000000000000000000000"))
pitm = box(b"pitm", bytes.fromhex("000000000001"))
infe = box(b"infe", bytes.fromhex("02000000") + struct.pack(">HH", 1, 0) + b"hvc1" + b"\x00")
iinf = box(b"iinf", bytes.fromhex("000000000001") + infe)
ftyp = box(b"ftyp", b"heic" + b"\x00\x00\x00\x00" + b"mif1heicmiaf")
iloc_head = bytes.fromhex("000000004400") + struct.pack(">HHHH", 1, 1, 0, 1)

def assemble(offset):
    iloc = box(b"iloc", iloc_head + struct.pack(">II", offset, len(sample)))
    meta = box(b"meta", b"\x00\x00\x00\x00" + hdlr + pitm + iloc + iinf + iprp)
    return ftyp + meta + box(b"mdat", sample)

blob = assemble(0)
blob = assemble(len(blob) - len(sample))
open(sys.argv[2], "wb").write(blob)
PY

ffmpeg -y -hide_banner -loglevel error \
  -f lavfi -i "color=c=0x336699:s=64x64:d=1,format=yuv420p" \
  -c:v libx264 -pix_fmt yuv420p -preset ultrafast -crf 32 -an \
  -metadata title="Albumen CC0 synthetic live photo" \
  -metadata artist="Albumen Project" \
  -metadata copyright="CC0-1.0" \
  "$work/IMG_LIVE.mov" >/dev/null

exiftool -overwrite_original \
  -Keys:ContentIdentifier="$CONTENT_ID" \
  "$work/IMG_LIVE.mov" >/dev/null

# Apple maker notes and a QuickTime Keys box cannot be created in a HEIC that
# does not already have them. Try those writes first, on a copy, and keep the
# first tag ExifTool actually stores.
clean="$work/IMG_LIVE.heic"
heic_tag=""
for tag in Apple:ContentIdentifier Keys:ContentIdentifier XMP-dc:Identifier; do
  cp "$clean" "$work/try.heic"
  exiftool -overwrite_original "-${tag}=${CONTENT_ID}" "$work/try.heic" >/dev/null
  if exiftool -s3 -"$tag" "$work/try.heic" | grep -Fxq "$CONTENT_ID"; then
    heic_tag="$tag"
    mv "$work/try.heic" "$out/IMG_LIVE.heic"
    break
  fi
done
if [[ -z "$heic_tag" ]]; then
  echo "exiftool stored none of Apple:ContentIdentifier, Keys:ContentIdentifier, XMP-dc:Identifier" >&2
  exit 1
fi
mv "$work/IMG_LIVE.mov" "$out/IMG_LIVE.mov"
printf '%s\n' "$heic_tag"
