# Public fixture licenses

`ingest/tests/public_fixtures.rs` downloads these files at test time and refuses a SHA-256 mismatch. The bytes are not committed. None of them is a personal photo, and `ALBUMEN_REAL_CORPUS` stays unset.

## HEIC

| | |
| --- | --- |
| File | `exif-gps-date.heic` |
| Source | https://heic.fyi/samples/exif-gps-date.heic |
| Page | https://heic.fyi/heic-sample-files |
| License | CC0 1.0 Universal. The set page and https://heic.fyi/samples/LICENSE.txt dedicate the files to the public domain. |
| SHA-256 | `01fd9fa28d09885d8482d01a5b36d88dc89fde3b7cfeaca50b70f8314d2c63d1` |
| Size | 191464 bytes |

ExifTool 13.59 reports EXIF, an XMP packet, and a GPS block. The publisher describes the file as a rendered chart, not a photograph, and states that the GPS position is fake. The same page says the set does not include Apple maker notes, Live Photo links, or photographs. There is no IPTC group.

## Apple ProRAW / DNG

| | |
| --- | --- |
| File | raw.pixls.us id 4264, `IMG_1361.DNG` |
| Source | https://raw.pixls.us/getfile.php/4264/raw/IMG_1361.DNG |
| License | CC0 1.0 Universal. The raw.pixls.us repository row for id 4264 (https://raw.pixls.us/json/getrepository.php?set=all) links https://creativecommons.org/publicdomain/zero/1.0/ and publishes the same SHA-256. Uploads require a public-domain dedication. |
| SHA-256 | `e91e77a4533ed7cce551d83330676ea5c47dd5e55fb38adda7819366afdbdfc2` |
| Size | 29195592 bytes |

This is an Apple iPhone 12 Pro ProRAW (DNG) photograph. ExifTool 13.59 reports Apple maker notes (the `Apple` group, including `MakerNoteVersion`), an XMP packet, and a GPS block. It reports no IPTC group. The file is a real photograph with real GPS, so the bytes stay out of this tree and the test fetches them.

## Live Photo pair — not found

No CC0, CC-BY, or public-domain HEIC plus MOV pair with one shared content identifier was found.

- The heic.fyi CC0 set states that it does not include Live Photo links or Apple maker notes.
- [stoffis-git/livephoto-check](https://github.com/stoffis-git/livephoto-check) is MIT code. Its repository does not publish the camera-captured HEIC or MOV it describes.
- Public iPhone Live Photo samples, including the PhotoPrism iPhone X set, have no stated media license. They were not fetched.
- ExifTool cannot create Apple maker notes, or a QuickTime `Keys:ContentIdentifier`, in a HEIC that does not already have them. Pairing a generated MOV with `exif-gps-date.heic` would not be a Live Photo pair.
- [immich-app/test-assets](https://github.com/immich-app/test-assets) is AGPL-3.0. Nothing from it was copied.

## IPTC — not in these license-clean files

Neither file above has an IPTC group under ExifTool 13.59 (`-a -G1`). The IPTC Photo Metadata reference JPEG at https://iptc.org/std/photometadata/examples/IPTC-PhotometadataRef-Std2025.1.jpg does contain IPTC, and its copyright notice is `Copyright (Notice) 2025.1 IPTC - www.iptc.org`. That is not CC0, CC-BY, or public domain, so it is not a fixture. The synthetic CC0 JPEG in `fixtures/jpeg-gps/` still carries IPTC through the existing corpus round-trip.
