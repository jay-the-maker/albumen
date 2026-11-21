---
name: verify-albumen-gui
description: Drive the Albumen local web GUI (`albumen gui`) in headless Chrome, take screenshots, and prove the library was not changed. Use after any change to cli/src/gui/ (page.html, mod.rs, overlay, thumbnails), before claiming a GUI feature works, or to capture review screenshots.
---

# Verify the Albumen GUI

The GUI is a page served by `albumen gui` on 127.0.0.1. It reads a photo
folder (`--library`, never written) and keeps its own state in `--work`.
Proof means: drive the real page the way a person would, save screenshots,
and show the library fingerprint is identical before and after.

Feature map: [features/README.md](features/README.md). A proof that covers
one entry point is incomplete when the map lists others.

All helpers are in `scripts/` next to this file. Run them from the repo root through their interpreter (`bash`, `python3`, `node`): a checkout may not keep the executable bit.

## Launch

```bash
S=.cursor/skills/verify-albumen-gui/scripts
export PATH="$(scripts/install-exiftool.sh):$PATH"      # pinned ExifTool, for Check photos
cargo build --locked -p albumen                          # binary at $CARGO_TARGET_DIR/debug/albumen
R=$(mktemp -d /tmp/albumen-verify-XXXX)
python3 $S/make-library.py "$R/lib" --odd               # 26 JPEGs, 4 events, EXIF dates/cameras/GPS; + symlink + control-char name
python3 $S/fingerprint.py "$R/lib" > "$R/before.sha"
bash $S/run-gui.sh start "$R/lib" "$R/work" "$R/run"          # prints "ready: http://127.0.0.1:PORT/"
```

Ready means `GET /api/library` answered 200, not just that the process is up.
`--port 0` gives each run its own port, so runs never share an instance.
Never point this at a real library: copy photos into `$R/lib` first.
`make-library.py --scale 40000` builds a large, EXIF-free library for grid
performance checks.

## Doctor

```bash
bash $S/run-gui.sh doctor "$R/run"
# ok: serving /tmp/albumen-verify-XXXX/lib; 27 photos shown of 28 files; skipped 2
```

FAIL means the pid is gone, the port does not answer, or it serves another
folder. Stop and relaunch; do not drive it.

## Drive

`scripts/drive.mjs` is a dependency-free DevTools client (Node 22+, Chrome or
Chromium; `CHROME=` overrides the path). It sends real mouse and key events.

```bash
U=$(cat "$R/run/url")
node $S/drive.mjs --url "$U" --out "$R/shots" \
  --step '{"goto":"#events"}'               --step '{"shot":"events"}' \
  --step '{"dblclick":"#grid .tile.ev"}'    --step '{"shot":"event-open"}' \
  --step '{"dblclick":"#grid figure.tile"}' --step '{"waitFor":"#viewer-img"}' --step '{"shot":"viewer"}'
```

Longer runs go in a JSON file (`--steps steps.json`). Step kinds and the
selectors each feature uses are in the drive.mjs header and in each feature
file. Stable handles: `data-s` on sidebar buttons (`[data-s=events]`,
`[data-s=all]`, ...), ids (`#grid`, `#viewer`, `#search`, `#size`, `#info`),
`.tile.ev` for an event tile, `figure.tile` for a photo, and
`<html data-ready="1">` after the first render. Hash routes: `#events`,
`#all`, `#recent`, `#photos`, `#backup`, `#health`, `#proof`.

Screenshots wait for `data-ready` and for images in the viewport to finish,
at most 8 s, and record `images_still_loading` in `log.jsonl`. They never
wait on network idle or virtual time: Chrome's `--screenshot
--virtual-time-budget` against the Events view timed out three times in
round 1, most likely because preview requests kept virtual time from
advancing (root cause not confirmed: Unknown).

Any console error or page exception fails the run (exit 1), and a failed
step saves `failed-<n>.png`.

## Evidence

- Screenshots and `log.jsonl` in `$R/shots` (or the `--out` you pass). Copy
  the ones you report somewhere that outlives `$R`.
- The library check, after `stop`:
  `python3 $S/fingerprint.py "$R/lib" > "$R/after.sha" && cmp "$R/before.sha" "$R/after.sha"`.
  Report the `total` line of both.
- Side effects: what appeared in `$R/work` (`scan-*.jsonl`, `thumbs/`, `overlay.json`).
- Standards: use the real user path (clicks, keys, the page's own buttons),
  not internal endpoints called by hand; capture the action and the result,
  not only the final screen. The automated checks are
  `cargo test --locked -p albumen --test gui --test gui_stdout_closed --test gui_lessons`.

## Cleanup

```bash
bash $S/run-gui.sh stop "$R/run"      # kills only the pid this run started
```

`drive.mjs` kills the Chrome it started and removes its temporary profile.
Never `pkill albumen` or `pkill chrome`: another agent's run or the user's
own browser may be running. Screenshots stay in `--out`; remove `$R/lib` and
`$R/work` only when you no longer need them.
