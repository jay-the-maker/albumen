#!/usr/bin/env python3
"""Assemble synthetic container fixtures.

The bytes are generated here. Nothing in this file is copied from a camera
file or from another photo library.

Motion Photo: a JPEG, then an MP4 appended after the JPEG EOI. XMP carries
both the older GCamera MicroVideo offset and the Container MotionPhoto item
length. Both numbers are the MP4 size in bytes, measured from the end.

Ultra HDR: a JPEG with a synthetic ICC profile, HDR gain-map XMP, and an MPF
segment whose second image is a grayscale gain-map JPEG. This is the Android
Ultra HDR shape. It is not an Apple HEIC auxiliary gain map; libheif's
encoder cannot write one, and that limit is documented in fixtures/README.md.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path


def s15(value: float) -> bytes:
    encoded = int(round(value * 65536))
    return struct.pack(">i", encoded)


def pad4(blob: bytes) -> bytes:
    extra = (-len(blob)) % 4
    return blob + (b"\x00" * extra)


def xyz_type(x: float, y: float, z: float) -> bytes:
    return b"XYZ " + b"\x00" * 4 + s15(x) + s15(y) + s15(z)


def curv_gamma(gamma: float) -> bytes:
    # u8.8 gamma. One entry is the ICC shortcut for a pure gamma curve.
    encoded = int(round(gamma * 256))
    return b"curv" + b"\x00" * 4 + struct.pack(">IH", 1, encoded)


def text_type(text: str) -> bytes:
    raw = text.encode("ascii") + b"\x00"
    return b"text" + b"\x00" * 4 + pad4(raw)


def desc_type(text: str) -> bytes:
    ascii_text = text.encode("ascii") + b"\x00"
    body = struct.pack(">I", len(ascii_text)) + ascii_text
    body = pad4(body)
    # Unicode language code and count, then an empty Mac script code.
    body += struct.pack(">II", 0, 0)
    body += struct.pack(">HB", 0, 0) + (b"\x00" * 67)
    return b"desc" + b"\x00" * 4 + pad4(body)


def synthetic_icc() -> bytes:
    """RGB display profile with sRGB-like primaries. Written by us, not copied."""
    tags = {
        "desc": desc_type("Albumen synthetic sRGB-like display profile"),
        "cprt": text_type("CC0-1.0 Albumen fixture. Synthetic ICC profile."),
        "wtpt": xyz_type(0.9642, 1.0, 0.8249),
        "rXYZ": xyz_type(0.4360, 0.2225, 0.0139),
        "gXYZ": xyz_type(0.3851, 0.7169, 0.0971),
        "bXYZ": xyz_type(0.1431, 0.0606, 0.7141),
        "rTRC": curv_gamma(2.2),
        "gTRC": curv_gamma(2.2),
        "bTRC": curv_gamma(2.2),
    }
    order = ["desc", "cprt", "wtpt", "rXYZ", "gXYZ", "bXYZ", "rTRC", "gTRC", "bTRC"]
    header_size = 128
    table_size = 4 + len(order) * 12
    blob = bytearray(header_size + table_size)
    cursor = header_size + table_size
    entries = bytearray()
    entries += struct.pack(">I", len(order))
    for name in order:
        data = tags[name]
        entries += name.encode("ascii") + struct.pack(">II", cursor, len(data))
        blob += data
        cursor += len(data)
    blob[header_size : header_size + len(entries)] = entries
    size = len(blob)
    blob[0:4] = struct.pack(">I", size)
    blob[8:12] = b"\x02\x20\x00\x00"  # version 2.2.0
    blob[12:16] = b"mntr"
    blob[16:20] = b"RGB "
    blob[20:24] = b"XYZ "
    blob[24:36] = struct.pack(">HHHHHH", 2026, 1, 1, 0, 0, 0)
    blob[36:40] = b"acsp"
    blob[40:44] = b"alb "
    blob[68:80] = s15(0.9642) + s15(1.0) + s15(0.8249)
    blob[80:84] = b"albm"
    return bytes(blob)


def app_segment(marker: bytes, payload: bytes) -> bytes:
    if len(payload) + 2 > 65535:
        raise SystemExit(f"JPEG segment is too large: {len(payload)}")
    return marker + struct.pack(">H", len(payload) + 2) + payload


def xmp_app1(packet: str) -> bytes:
    return app_segment(b"\xff\xe1", b"http://ns.adobe.com/xap/1.0/\x00" + packet.encode("utf-8"))


def icc_app2(profile: bytes) -> bytes:
    payload = b"ICC_PROFILE\x00" + bytes((1, 1)) + profile
    return app_segment(b"\xff\xe2", payload)


def mpf_tiff(primary_size: int, gain_size: int, gain_offset: int) -> bytes:
    """CIPA DC-007 MPF. Offsets are relative to this TIFF header."""

    def entry(tag: int, typ: int, count: int, value: bytes) -> bytes:
        if len(value) != 4:
            raise SystemExit("IFD inline value must be 4 bytes")
        return struct.pack(">HHI", tag, typ, count) + value

    def mp_entry(attr: int, size: int, offset: int, dep1: int = 0, dep2: int = 0) -> bytes:
        return struct.pack(">IIIHH", attr, size, offset, dep1, dep2)

    # 0x00030000 is Baseline MP Primary Image. 0x40000000 is the dependent-child
    # flag. The gain map's image type stays undefined; the HDR gain-map XMP names it.
    entries = mp_entry(0x00030000, primary_size, 0, dep1=2) + mp_entry(
        0x40000000, gain_size, gain_offset
    )
    ifd = struct.pack(">H", 3)
    ifd += entry(0xB000, 7, 4, b"0100")
    ifd += entry(0xB001, 4, 1, struct.pack(">I", 2))
    ifd += entry(0xB002, 7, 32, struct.pack(">I", 50))
    ifd += struct.pack(">I", 0)
    header = b"MM" + struct.pack(">HI", 42, 8)
    blob = header + ifd + entries
    if len(blob) != 82:
        raise SystemExit(f"unexpected MPF TIFF size {len(blob)}")
    return blob


def jpeg_body(path: Path) -> bytes:
    data = path.read_bytes()
    if not data.startswith(b"\xff\xd8"):
        raise SystemExit(f"{path} is not a JPEG")
    return data[2:]


def motion_xmp(video_size: int) -> str:
    return f"""<?xpacket begin="\ufeff" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Albumen Fixture Generator">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:GCamera="http://ns.google.com/photos/1.0/camera/"
    xmlns:Container="http://ns.google.com/photos/1.0/container/"
    xmlns:Item="http://ns.google.com/photos/1.0/container/item/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    GCamera:MotionPhoto="1"
    GCamera:MotionPhotoVersion="1"
    GCamera:MotionPhotoPresentationTimestampUs="500000"
    GCamera:MicroVideo="1"
    GCamera:MicroVideoVersion="1"
    GCamera:MicroVideoOffset="{video_size}"
    GCamera:MicroVideoPresentationTimestampUs="500000">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Synthetic CC0 Android motion photo. JPEG with an MP4 appended after the JPEG EOI.</rdf:li>
    </rdf:Alt>
   </dc:description>
   <Container:Directory>
    <rdf:Seq>
     <rdf:li rdf:parseType="Resource">
      <Container:Item Item:Mime="image/jpeg" Item:Semantic="Primary" Item:Length="0" Item:Padding="0"/>
     </rdf:li>
     <rdf:li rdf:parseType="Resource">
      <Container:Item Item:Mime="video/mp4" Item:Semantic="MotionPhoto" Item:Length="{video_size}" Item:Padding="0"/>
     </rdf:li>
    </rdf:Seq>
   </Container:Directory>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"""


def hdr_xmp() -> str:
    return """<?xpacket begin="\ufeff" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Albumen Fixture Generator">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
    hdrgm:Version="1.0"
    hdrgm:GainMapMin="0.0"
    hdrgm:GainMapMax="3.0"
    hdrgm:Gamma="1.0"
    hdrgm:OffsetSDR="0.015625"
    hdrgm:OffsetHDR="0.015625"
    hdrgm:HDRCapacityMin="0.0"
    hdrgm:HDRCapacityMax="3.0"
    hdrgm:BaseRenditionIsHDR="False">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Synthetic CC0 Ultra HDR JPEG. Primary image, synthetic ICC profile, XMP gain-map description, and an MPF secondary gain-map JPEG. Not a photograph and not an Apple HEIC gain map.</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"""


def assemble_motion_photo(jpeg_path: Path, mp4_path: Path, dest: Path) -> None:
    video = mp4_path.read_bytes()
    if b"ftyp" not in video[:32]:
        raise SystemExit(f"{mp4_path} does not look like an MP4")
    # The JPEG body is wrapped again so the XMP is inside the file and the
    # MP4 begins at the first byte after FFD9.
    body = jpeg_body(jpeg_path)
    primary = b"\xff\xd8" + xmp_app1(motion_xmp(len(video))) + body
    if not primary.endswith(b"\xff\xd9"):
        raise SystemExit("motion photo primary JPEG has no EOI")
    dest.write_bytes(primary + video)
    written = dest.read_bytes()
    if written[-len(video) :] != video:
        raise SystemExit("motion photo trailer was not appended bit-exactly")
    if written[-(len(video) + 2) : -len(video)] != b"\xff\xd9":
        raise SystemExit("motion photo MP4 does not start immediately after EOI")


def assemble_ultra_hdr(primary_jpeg: Path, gainmap_jpeg: Path, dest: Path) -> None:
    body = jpeg_body(primary_jpeg)
    gain = gainmap_jpeg.read_bytes()
    if not gain.startswith(b"\xff\xd8") or not gain.endswith(b"\xff\xd9"):
        raise SystemExit("gain map is not a complete JPEG")
    xmp_seg = xmp_app1(hdr_xmp())
    icc_seg = icc_app2(synthetic_icc())
    # TIFF payload is fixed at 82 bytes. The APP2 marker, length, and "MPF\0"
    # sit in front of it, so the TIFF header is 8 bytes into the segment payload
    # and 4 bytes after the length field.
    mpf_seg_len = 2 + 4 + 82  # length field + identifier + tiff
    mpf_seg = b"\xff\xe2" + struct.pack(">H", mpf_seg_len) + b"MPF\x00" + (b"\x00" * 82)
    primary = b"\xff\xd8" + xmp_seg + icc_seg + mpf_seg + body
    if not primary.endswith(b"\xff\xd9"):
        raise SystemExit("ultra hdr primary JPEG has no EOI")
    tiff_start = 2 + len(xmp_seg) + len(icc_seg) + 2 + 2 + 4
    gain_offset = len(primary) - tiff_start
    tiff = mpf_tiff(len(primary), len(gain), gain_offset)
    mpf_at = 2 + len(xmp_seg) + len(icc_seg)
    rebuilt = primary[: mpf_at + 8] + tiff + primary[mpf_at + 8 + len(tiff) :]
    if len(rebuilt) != len(primary):
        raise SystemExit("MPF splice changed the primary size")
    dest.write_bytes(rebuilt + gain)


def main() -> None:
    if len(sys.argv) != 7:
        raise SystemExit(
            "usage: assemble.py motion-base.jpg motion.mp4 motion-out.jpg "
            "hdr-base.jpg gainmap.jpg hdr-out.jpg"
        )
    _, motion_base, motion_mp4, motion_out, hdr_base, gainmap, hdr_out = sys.argv
    assemble_motion_photo(Path(motion_base), Path(motion_mp4), Path(motion_out))
    assemble_ultra_hdr(Path(hdr_base), Path(gainmap), Path(hdr_out))


if __name__ == "__main__":
    main()
