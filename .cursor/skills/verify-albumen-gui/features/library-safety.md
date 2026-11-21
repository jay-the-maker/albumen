# Read-only safety

What a person sees that tells them the library is only read, and what the
GUI refuses.

## Sub-features

- Green note: "Viewing your photos read-only, on this computer…" for a real
  library; the yellow demo banner only with `--demo`.
- Amber note when entries were skipped: "N items in this folder could not
  be shown and were skipped (1 symlink, 1 unsafe name): …". The other
  photos are still shown.
- Start refuses a `--work` that is the library, inside it, or hard-linked
  into it (exit non-zero, nothing created).
- `/img` and `/thumb` refuse `..`, absolute paths and non-images.

## How to get to it (user POV)

Visible at the top of every screen.

## Driving it with drive.mjs

```json
[{"goto":"#events"},
 {"assert":"!document.getElementById('real-note').hidden"},
 {"assert":"/2 items .* skipped \\(1 symlink, 1 unsafe name\\)/.test(document.getElementById('skip-note').textContent)"},
 {"shot":"safety-notes"}]
```

Proof: fingerprint identical before and after (SKILL.md), and `--work`
holds only `thumbs/` (plus `scan-*.jsonl` if Check photos ran).

## Gotchas

- Needs a library built with `make-library.py --odd` for the amber note.
