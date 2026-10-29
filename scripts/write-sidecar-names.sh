#!/usr/bin/env bash
# Issue #42: sidecar naming edge cases, written onto copies of committed CC0
# fixtures. Image-ExifTool 13.59 produces the same bytes on every run. No GPS
# is added; the RAW+JPEG copy has its GPS removed.
set -euo pipefail
trap 'echo "write-sidecar-names.sh failed: status $? line ${BASH_LINENO[0]:-?}" >&2' ERR

dest="${1:?usage: write-sidecar-names.sh DIR}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$dest"
dest="$(cd "$dest" && pwd)"

if ! command -v exiftool >/dev/null 2>&1; then
  echo "exiftool is required to write the sidecar-name fixtures" >&2
  exit 1
fi

# New numbers: the corpus round-trip stores by filename.
cp "$root/fixtures/xmp/IMG_0004.jpg" "$dest/IMG_0012.JPG"
cp "$root/fixtures/aae/IMG_0008.jpg" "$dest/IMG_0013.jpg"
cp "$root/fixtures/raw-jpeg/IMG_0003.dng" "$dest/IMG_0014.dng"
cp "$root/fixtures/raw-jpeg/IMG_0003.jpg" "$dest/IMG_0014.jpg"

describe() {
  exiftool -overwrite_original -m \
    -EXIF:ModifyDate="2012:06:15 10:30:00" \
    -ImageDescription="$1" \
    "$2" >/dev/null
}
describe "Synthetic CC0 JPEG with a full-name sidecar (IMG_0012.JPG.xmp). Not a photograph." "$dest/IMG_0012.JPG"
describe "Synthetic CC0 JPEG with an uppercase .XMP sidecar. Not a photograph." "$dest/IMG_0013.jpg"
describe "Synthetic CC0 DNG paired with IMG_0014.jpg and IMG_0014.xmp. Not a camera capture." "$dest/IMG_0014.dng"
exiftool -overwrite_original -m -gps:all= "$dest/IMG_0014.jpg" >/dev/null
describe "Synthetic CC0 JPEG paired with IMG_0014.dng and IMG_0014.xmp. Not a photograph." "$dest/IMG_0014.jpg"

sidecar() {
  cat > "$2" <<EOF
<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Albumen Fixture Generator">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">$1</rdf:li>
    </rdf:Alt>
   </dc:description>
   <dc:rights>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">CC0 1.0 Universal</rdf:li>
    </rdf:Alt>
   </dc:rights>
   <xmp:Rating>4</xmp:Rating>
   <xmp:Label>albumen-fixture</xmp:Label>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
EOF
}
sidecar "Albumen CC0 full-name sidecar fixture. Not a photograph." "$dest/IMG_0012.JPG.xmp"
sidecar "Albumen CC0 uppercase sidecar fixture. Not a photograph." "$dest/IMG_0013.XMP"
sidecar "Albumen CC0 RAW+JPEG sidecar fixture. Not a photograph." "$dest/IMG_0014.xmp"
find "$dest" -name '*_original' -delete
