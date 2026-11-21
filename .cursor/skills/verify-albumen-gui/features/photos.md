# Photos and Last Import

## Sub-features

- Photos: every previewable image, grouped under one header per event with
  its date range, count and where the dates came from (photo dates from
  EXIF, or file dates).
- Captions show the date and time (`YYYY-MM-DD HH:MM`), not the file name.
- Last Import: images whose file modified time is within 7 days of the
  newest file, newest first.
- Previews are 480 px JPEGs from `/thumb`, cached in `--work/thumbs`;
  images load lazily.
- HEIC, RAW, video and other files are counted in the summary line but not
  previewed.

## How to get to it (user POV)

Sidebar > Library > Photos, or Last Import.

## Driving it with drive.mjs

```json
[{"goto":"#all"},
 {"assert":"document.querySelectorAll('#grid section.group').length===5"},
 {"assert":"document.getElementById('photo-count').textContent==='27 photos'"},
 {"shot":"photos"},
 {"click":"[data-s=recent]"},
 {"shot":"last-import"}]
```

Proof: the count in the toolbar equals `images.length` from `/api/library`,
and every group header count adds up to it.

## Gotchas

- A library made seconds ago has every file in Last Import.
