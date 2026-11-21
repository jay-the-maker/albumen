# Viewer

A full-window dark view of one photo at full resolution (from `/img`), with
previous and next buttons, a caption (file name, date, position "k of n")
and a close button.

## Sub-features

- Double-click a photo tile to open it.
- Arrow keys and the ‹ › buttons step through the current grid's order.
- Escape, the Close button or a click on the picture closes it.

## How to get to it (user POV)

Double-click any photo in Photos, an open event, or Last Import.

## Driving it with drive.mjs

```json
[{"goto":"#all"},
 {"dblclick":"#grid figure.tile"},
 {"waitFn":"document.getElementById('viewer-img').complete"},
 {"assert":"/1 of 27/.test(document.getElementById('viewer-cap').textContent)"},
 {"key":"ArrowRight"},
 {"assert":"/2 of 27/.test(document.getElementById('viewer-cap').textContent)"},
 {"shot":"viewer"},
 {"key":"Escape"},
 {"assert":"document.getElementById('viewer').hidden"}]
```

## Gotchas

- `/img` serves the original bytes; large originals take longer than previews.
