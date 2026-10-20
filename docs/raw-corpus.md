# CC0 camera RAW test corpus

Wikimedia Commons can't host camera RAW, so RAW gets a corpus of its own: 10 files from [raw.pixls.us](https://raw.pixls.us), every one CC0, 201,436,872 bytes in total. Proposal: issue #175.

| Format | Camera |
|---|---|
| CR2 | Canon EOS 5D Mark III |
| CR3 | Canon EOS R6 |
| NEF | Nikon D750 |
| ARW | Sony ILCE-7M3 |
| RAF | Fujifilm X-T3 |
| ORF | Olympus E-M1 Mark II |
| RW2 | Panasonic DC-G9 |
| PEF | Pentax K-1 Mark II |
| DNG | Google Pixel 7 Pro, DJI FC4382 |

The two DNGs carry GPS. The CC0 ProRAW DNG (id 4264) in `ingest/tests/public_fixtures.rs` stays where it is. **No media is committed:** the repository holds only `corpus/raw.manifest.json` and the generated `corpus/RAW_ATTRIBUTION.md`.

## License and author, recorded per file

The license is read from each file's own row in the raw.pixls.us repository listing (`json/getrepository.php?set=all`), never from a site-wide assumption. Every row used links the CC0 1.0 deed, and the manifest quotes that row in `license_source`. Rows under CC BY-NC-SA 4.0 (143 of them, mostly the rawsamples.ch import) were excluded. raw.pixls.us does not publish uploader names, so `author` says exactly that. Each row's published SHA-256 matched the download and is stored in `publisher_checksums`.

## How CI runs it

The `RAW corpus round-trip` job follows the same steps as the Commons and media jobs:
1. Restores `actions/cache`, keyed by `hashFiles('corpus/raw.manifest.json')` with no restore-keys.
2. On a cache miss, runs `--corpus raw fetch`.
3. Always runs `verify`: any size, SHA-1 or SHA-256 mismatch, or any missing or extra file, fails hard and is never re-run.
4. Runs `cargo test -p albumen-ingest --test commons_corpus raw_corpus_round_trip` with `ALBUMEN_RAW_CORPUS_REQUIRED=1`.

The round-trip checks:
- the bytes are bit-exact by SHA-256, and the mtime is kept;
- every embedded tag is equal (computed `Composite:` tags are left out);
- the family-0 group counts (EXIF, MakerNotes, XMP and the vendor RAW groups) equal both the original's and the manifest's;
- the camera model and GPS presence match.

Default scan, index and search stay offline.

## Attribution

<!-- attribution:start -->
These files are **not** in this repository. CI downloads them at run time
from raw.pixls.us, verifies them against `raw.manifest.json`
(size, SHA-1, SHA-256) and uses them unmodified, only to test that
Albumen copies originals bit-exactly and keeps their metadata.
No file was changed. Licenses belong to the authors listed below.

| # | File (raw.pixls.us camera page) | Author | License |
|---|---|---|---|
| 1 | [raw.pixls.us id 771: Canon - EOS 5D Mark III.CR2](https://raw.pixls.us/data/Canon/EOS%205D%20Mark%20III/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 2 | [raw.pixls.us id 4661: Canon - EOS R6 - 3:2.CR3](https://raw.pixls.us/data/Canon/EOS%20R6/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 3 | [raw.pixls.us id 897: Nikon - D750 - 12bit 12bit compressed (Lossless) (3:2).NEF](https://raw.pixls.us/data/Nikon/D750/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 4 | [raw.pixls.us id 2415: Sony - ILCE-7M3 - 12bit 12bit compressed (3:2).ARW](https://raw.pixls.us/data/Sony/ILCE-7M3/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 5 | [raw.pixls.us id 2783: Fujifilm - X-T3 - 14bit 14bit compressed (3:2).RAF](https://raw.pixls.us/data/Fujifilm/X-T3/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 6 | [raw.pixls.us id 1993: Olympus - E-M1MarkII - 16bit (4:3).ORF](https://raw.pixls.us/data/Olympus/E-M1MarkII/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 7 | [raw.pixls.us id 2348: Panasonic - DC-G9 - 4:3.RW2](https://raw.pixls.us/data/Panasonic/DC-G9/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 8 | [raw.pixls.us id 7133: Pentax - K-1 Mark II - 14bit (3:2).PEF](https://raw.pixls.us/data/Pentax/K-1%20Mark%20II/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 9 | [raw.pixls.us id 6168: Google - Pixel 7 Pro - 16bit (4:3).dng](https://raw.pixls.us/data/Google/Pixel%207%20Pro/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 10 | [raw.pixls.us id 7823: DJI - FC4382 - 16bit (4:3).DNG](https://raw.pixls.us/data/DJI/FC4382/) | not published by raw.pixls.us (anonymous public-domain upload) | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |

Attribution strings, as required by the CC BY and CC BY-SA licenses:

- None required: every file above is CC0 or public domain.
<!-- attribution:end -->
