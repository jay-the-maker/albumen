# CC0 HEIC, MOV and MP4 test corpus

Wikimedia Commons can't host HEIC, MOV or MP4 (see [commons-corpus.md](commons-corpus.md)), so these formats get a corpus of their own. It holds 6 camera-captured files from the Internet Archive, every one CC0, 63,117,112 bytes in total. Proposal: issue #176.

| Kind | Files | Sources |
|---|---|---|
| HEIC | 2 | iPhone 7, iPhone 11 |
| MOV | 3 | iPad Air (with GPS), iPhone 4S, Nikon 1 V1 |
| MP4 | 1 | Panasonic DMC-FZ150 |

The HEIC and MOV files carry real EXIF, QuickTime, MakerNotes and GPS blocks. **No media is committed:** the repository holds only `corpus/media.manifest.json` and the generated `corpus/MEDIA_ATTRIBUTION.md`.

## License, recorded per file

Each archive.org item states its own license in its `licenseurl` metadata field, and every file here comes from an item whose `licenseurl` is the CC0 1.0 deed. The manifest records that statement for every file in `license_source`, along with the item's `creator` field as `author`. One item lists no creator, and the manifest says so instead of inventing one. The manifest also stores archive.org's published SHA-1 and MD5 (`publisher_checksums`); both matched the download when the manifest was written.

## How CI runs it

The `Media corpus round-trip` job follows the same steps as the Commons job:
1. Checks the attribution files with `--corpus media attribution --check`.
2. Restores `actions/cache`, keyed by `hashFiles('corpus/media.manifest.json')` with no restore-keys.
3. On a cache miss, runs `fetch`.
4. Always runs `verify`: any size, SHA-1 or SHA-256 mismatch, or any missing or extra file, fails hard and is never re-run.
5. Runs `cargo test -p albumen-ingest --test commons_corpus media_corpus_round_trip` with `ALBUMEN_MEDIA_CORPUS_REQUIRED=1`.

The round-trip makes the same checks as the Commons corpus:
- the bytes are bit-exact by SHA-256, and the mtime is kept;
- every embedded tag is equal (computed `Composite:` tags are left out);
- the family-0 group counts equal both the original's and the manifest's;
- the camera model, read from the tag named in `expect.model_tag`, matches;
- GPS presence matches;
- the download is unchanged afterwards.

Default scan, index and search stay offline.

## Attribution

<!-- attribution:start -->
These files are **not** in this repository. CI downloads them at run time
from the Internet Archive, verifies them against `media.manifest.json`
(size, SHA-1, SHA-256) and uses them unmodified, only to test that
Albumen copies originals bit-exactly and keeps their metadata.
No file was changed. Licenses belong to the authors listed below.

| # | File (archive.org item) | Author | License |
|---|---|---|---|
| 1 | [08-ea-0-a-84-063-e-4-f-33-8-b-95-3-c-0-ed-11-fcd-47/08EA0A84-063E-4F33-8B95-3C0ED11FCD47.heic](https://archive.org/details/08-ea-0-a-84-063-e-4-f-33-8-b-95-3-c-0-ed-11-fcd-47) | Tyler Jones | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 2 | [img-0863/IMG_0863.HEIC](https://archive.org/details/img-0863) | not listed on the archive.org item | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 3 | [pkvideofootage/IMG_0383.MOV](https://archive.org/details/pkvideofootage) | powerKitten | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 4 | [ZooChile/IMG_1040.MOV](https://archive.org/details/ZooChile) | Andrés Delgado-Ron | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 5 | [independencecup/1/NMS_0001.MOV](https://archive.org/details/independencecup) | julian correa | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 6 | [PanasonicDmc-fz150Mp4-original-videoSchaerfeverlagerung/04_FZ150-Schaerfeverlagerung_focus-control_2.MP4](https://archive.org/details/PanasonicDmc-fz150Mp4-original-videoSchaerfeverlagerung) | consulting | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |

Attribution strings, as required by the CC BY and CC BY-SA licenses:

- None required: every file above is CC0 or public domain.
<!-- attribution:end -->
