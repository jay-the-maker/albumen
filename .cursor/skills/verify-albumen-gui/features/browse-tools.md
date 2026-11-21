# Search, thumbnail size, info pane

## Sub-features

- Search box (bottom left): filters photos by path, ISO date, time and
  camera; filters events by name and date.
- Size slider (bottom right): tile width 90 to 420 px.
- Info button toggles the right-hand pane. For a photo: file name, date and
  its source, size, camera (Unknown until Check photos), event, path. For an
  event: dates, photo count, size, where dates came from.
- Summary line (title bar): files, bytes, photos shown, and how many files
  are checked but not previewed.

## How to get to it (user POV)

The toolbar under any browsing view (Events, Photos, Last Import).

## Driving it with drive.mjs

```json
[{"goto":"#all"},
 {"type":"Garden","into":"#search"},
 {"assert":"document.getElementById('photo-count').textContent==='7 photos'"},
 {"eval":"(()=>{const s=document.getElementById('size');s.value=300;s.dispatchEvent(new Event('input'));})()"},
 {"click":"#grid figure.tile"},
 {"shot":"search-and-info"}]
```

## Gotchas

- The size slider has no keyboard step test yet; it is set through its input event.
