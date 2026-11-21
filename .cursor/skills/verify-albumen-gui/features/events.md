# Events

One tile per folder of photos (photos at the top level form an event named
after the library folder). Each tile shows the key photo (the earliest
photo), the event name, its ISO date range and a photo count.

## Sub-features

- Event tiles sorted by first photo date.
- Skim: moving the pointer across a tile flips its picture through the
  event's photos; leaving the tile shows the key photo again.
- Click selects the event and fills the info pane (dates, photos, size, where dates came from).
- Double-click opens the event; the title shows its name and a "‹ Events" back button appears.
- Search filters events by name or date.

## How to get to it (user POV)

Opening the GUI lands on Events. From elsewhere: sidebar > Library > Events.

## Driving it with drive.mjs

```json
[{"goto":"#events"},
 {"assert":"document.querySelectorAll('#grid .tile.ev').length===5","msg":"4 folders + top level"},
 {"shot":"events"},
 {"click":"#grid .tile.ev"},
 {"assert":"document.querySelector('#info h4').textContent.length>0"},
 {"dblclick":"#grid .tile.ev"},
 {"assert":"!document.getElementById('back').hidden"},
 {"shot":"event-open"},
 {"click":"#back"},
 {"assert":"document.getElementById('title').textContent==='Events'"}]
```

Proof: the tile count equals the number of folders holding images (plus the
top level if it has images), each caption reads `YYYY-MM-DD[ to YYYY-MM-DD] · N photos`, and the opened
event shows exactly N tiles.

## Gotchas

- Skimming needs `mouseMoved` events across the image; a click alone never skims.
- Dates are file dates until Check photos has run (the header says which).
