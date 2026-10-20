# Wikimedia Commons test corpus

Albumen's committed fixtures are synthetic. To exercise real camera files, CI also runs a pinned set of 12 files from Wikimedia Commons: 10 JPEGs from 10 camera models (Canon, Nikon ×2, Sony ×2, Fujifilm, Olympus, Google Pixel, Apple iPhone ×2) with real EXIF, XMP, IPTC, ICC, maker-note and GPS blocks, plus one WebM and one MPEG-PS video. That is 37,161,280 bytes in total. Proposal and review: issue #173.

**No media is committed.** The repository holds only `corpus/commons.manifest.json` and the generated `corpus/ATTRIBUTION.md`.

## What Commons cannot cover

Commons accepts only these upload types: tiff, tif, png, gif, jpg, jpeg, webp, xcf, pdf, mid, ogg, ogv, svg, djvu, stl, oga, flac, opus, wav, webm, mp3, midi, mpg, mpeg. HEIC, camera RAW, MP4 and MOV are not on Commons. They are covered by the synthetic fixtures and by `ingest/tests/public_fixtures.rs` (one heic.fyi HEIC, one raw.pixls.us DNG). Wider RAW, HEIC, MP4 and MOV sources are tracked in follow-up issues.

## Manifest

Each entry in `corpus/commons.manifest.json` has these fields:
- `local_name`: an ASCII name for the file on disk
- `title`, `page`, `url`: the Commons file page and the original upload URL
- `revision_timestamp`: when the pinned Commons revision was uploaded
- `size`, `sha1` (the same value the Commons API reports), `sha256`
- `license`, `license_url`, `author`, `attribution`
- `kind`: `jpeg` or `video`
- `expect`: `exif_model`, whether GPS is present, and ExifTool 13.59 family-0 group counts (`-G0 -a -u`, without File, System, Composite and ExifTool), recorded when the manifest was written

## How CI runs it

The `Commons corpus round-trip` job in `.github/workflows/ci.yml` runs these steps:
1. Installs the pinned ExifTool.
2. Runs `scripts/fetch-commons-corpus.py attribution --check`, which fails if `ATTRIBUTION.md` or the attribution section below no longer matches the manifest.
3. Restores `$RUNNER_TEMP/commons-corpus` with `actions/cache`. The key is `commons-corpus-v1-<hashFiles(corpus/commons.manifest.json)>` and there are no `restore-keys`, so a manifest change always means a clean download.
4. On a cache miss, `fetch` downloads every file from `upload.wikimedia.org` one at a time, with a descriptive User-Agent, and leaves each file read-only.
5. Runs `verify`, on a hit as well as a miss. Size, SHA-1 and SHA-256 must all match, and any missing or extra file fails the job.
6. Runs `cargo test -p albumen-ingest --test commons_corpus` with `ALBUMEN_COMMONS_CORPUS_REQUIRED=1`.

### Failure handling

| Failure | Log line | Re-run? |
|---|---|---|
| Hash or size mismatch | `commons-corpus mismatch name=… expected_sha256=… got=…` | Never. The fix is a manifest PR. |
| HTTP error | `commons-corpus fetch-failed` | Yes. This is an infrastructure failure: the job retries 429 and 5xx three times, and a manual re-run is allowed but must be stated. |

There is no fallback to an unpinned file.

## The round-trip test

For each of the 12 files, `ingest/tests/commons_corpus.rs` does the following:
1. Checks the downloaded original against the manifest SHA-256.
2. Stages a copy with a fixed nanosecond mtime, so the download itself is only ever read.
3. Runs `discover`, `copy_verify` into a `LocalStore`, then `get_verified` back out.
4. Requires all of the following:
   - the bytes are bit-exact (the stored, read-back and restored SHA-256 all equal the manifest);
   - the mtime is preserved;
   - the full `exiftool -G1 -a -u -ee` dump is identical between the staged file and the restored copy. Computed `Composite:` tags are left out: ExifTool can list the candidates of an ambiguous `Composite:LensID` in a different order on two reads of the same bytes;
   - the family-0 group counts (EXIF, XMP, IPTC, MakerNotes, ICC_Profile and so on) are equal to the original's and to `expect.tags`;
   - the camera model and GPS presence match;
   - the downloaded original is unchanged afterwards.
5. Prints one `commons-roundtrip name=… result=ok …` line per file.

Outside that CI job the round-trip test skips with a message. `commons_manifest_is_well_formed_and_attributed` always runs, offline. Default scan, index and search never touch the network: only the script above downloads, and only the CI job runs it.

To run it locally:

```sh
python3 scripts/fetch-commons-corpus.py fetch /tmp/commons-corpus
ALBUMEN_COMMONS_CORPUS=/tmp/commons-corpus cargo test -p albumen-ingest --test commons_corpus -- --nocapture
```

## Attribution

<!-- attribution:start -->
These files are **not** in this repository. CI downloads them at run time
from Wikimedia Commons, verifies them against `commons.manifest.json`
(size, SHA-1, SHA-256) and uses them unmodified, only to test that
Albumen copies originals bit-exactly and keeps their metadata.
No file was changed. Licenses belong to the authors listed below.

| # | File (Commons page) | Author | License |
|---|---|---|---|
| 1 | [2023-10-15 Fussball, Google Pixel Frauen-Bundesliga, RB Leipzig - VfL Wolfsburg 1DX 3310.jpg](https://commons.wikimedia.org/wiki/File:2023-10-15_Fussball,_Google_Pixel_Frauen-Bundesliga,_RB_Leipzig_-_VfL_Wolfsburg_1DX_3310.jpg) | Steffen Prößdorf | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| 2 | [Air France F-HEPB at Lisbon Portela Airport 2025 283.jpg](https://commons.wikimedia.org/wiki/File:Air_France_F-HEPB_at_Lisbon_Portela_Airport_2025_283.jpg) | Mike Peel (www.mikepeel.net) | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| 3 | [Argiope bruennichi 08Oct10.jpg](https://commons.wikimedia.org/wiki/File:Argiope_bruennichi_08Oct10.jpg) | 池田正樹 masaki ikeda | Public domain |
| 4 | [Balaclava as suggested fashion piece for winter 2018 - modelled by ModelTanja.jpg](https://commons.wikimedia.org/wiki/File:Balaclava_as_suggested_fashion_piece_for_winter_2018_-_modelled_by_ModelTanja.jpg) | Tobias ToMar Maier | [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) |
| 5 | [Chlosyne lacinia.jpg](https://commons.wikimedia.org/wiki/File:Chlosyne_lacinia.jpg) | Wilfredor | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 6 | [Faroese sheep Sumba 1.jpg](https://commons.wikimedia.org/wiki/File:Faroese_sheep_Sumba_1.jpg) | kallerna | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| 7 | [Koyaike Park from airplane window.jpg](https://commons.wikimedia.org/wiki/File:Koyaike_Park_from_airplane_window.jpg) | Grendelkhan | [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/) |
| 8 | [Malva moschata Mitterbach 02.jpg](https://commons.wikimedia.org/wiki/File:Malva_moschata_Mitterbach_02.jpg) | Uoaei1 | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| 9 | [Monte Serrat funicular 2017 03.webm](https://commons.wikimedia.org/wiki/File:Monte_Serrat_funicular_2017_03.webm) | Mike Peel (www.mikepeel.net) | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| 10 | [Snag diving.mpg](https://commons.wikimedia.org/wiki/File:Snag_diving.mpg) | Sezerpal | [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| 11 | [Un bus Colibri direction Miribel à l'intersection de la rue des Gravelles et de la rue des Andrés (Saint-Maurice-de-Beynost) en octobre 2021.jpg](https://commons.wikimedia.org/wiki/File:Un_bus_Colibri_direction_Miribel_%C3%A0_l%27intersection_de_la_rue_des_Gravelles_et_de_la_rue_des_Andr%C3%A9s_(Saint-Maurice-de-Beynost)_en_octobre_2021.jpg) | Benoît Prieur | [CC0](https://creativecommons.org/publicdomain/zero/1.0/) |
| 12 | [Wasp and spider 02.jpg](https://commons.wikimedia.org/wiki/File:Wasp_and_spider_02.jpg) | Tony Wills | [CC BY 2.5](https://creativecommons.org/licenses/by/2.5/) |

Attribution strings, as required by the CC BY and CC BY-SA licenses:

- "2023-10-15 Fussball, Google Pixel Frauen-Bundesliga, RB Leipzig - VfL Wolfsburg 1DX 3310.jpg" by Steffen Prößdorf, CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:2023-10-15_Fussball,_Google_Pixel_Frauen-Bundesliga,_RB_Leipzig_-_VfL_Wolfsburg_1DX_3310.jpg
- "Air France F-HEPB at Lisbon Portela Airport 2025 283.jpg" by Mike Peel (www.mikepeel.net), CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Air_France_F-HEPB_at_Lisbon_Portela_Airport_2025_283.jpg
- "Balaclava as suggested fashion piece for winter 2018 - modelled by ModelTanja.jpg" by Tobias ToMar Maier, CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Balaclava_as_suggested_fashion_piece_for_winter_2018_-_modelled_by_ModelTanja.jpg
- "Faroese sheep Sumba 1.jpg" by kallerna, CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Faroese_sheep_Sumba_1.jpg
- "Koyaike Park from airplane window.jpg" by Grendelkhan, CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Koyaike_Park_from_airplane_window.jpg
- "Malva moschata Mitterbach 02.jpg" by Uoaei1, CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Malva_moschata_Mitterbach_02.jpg
- "Monte Serrat funicular 2017 03.webm" by Mike Peel (www.mikepeel.net), CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Monte_Serrat_funicular_2017_03.webm
- "Snag diving.mpg" by Sezerpal, CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Snag_diving.mpg
- "Wasp and spider 02.jpg" by Tony Wills, CC BY 2.5 (https://creativecommons.org/licenses/by/2.5/), via Wikimedia Commons: https://commons.wikimedia.org/wiki/File:Wasp_and_spider_02.jpg
<!-- attribution:end -->
