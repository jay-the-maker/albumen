# Albumen GUI feature map

Every feature a person can reach in `albumen gui`, from their point of view.
Each file says what the feature is, how to reach it, how to drive it with
`scripts/drive.mjs`, and what end state proves it works. Keep it in step
with `cli/src/gui/page.html`: a new button or view gets a row here in the
same change.

| Feature | File | Reached from |
| --- | --- | --- |
| Events | [events.md](events.md) | Sidebar > Library > Events (default view) |
| Photos and Last Import | [photos.md](photos.md) | Sidebar > Library > Photos / Last Import |
| Viewer | [viewer.md](viewer.md) | Double-click any photo |
| Search, size, info | [browse-tools.md](browse-tools.md) | Bottom toolbar, right-hand info pane |
| Check photos | [check-photos.md](check-photos.md) | Sidebar > Backup > Check photos |
| Back up, Backup health, Proof | [backup.md](backup.md) | Sidebar > Backup |
| Read-only safety | [library-safety.md](library-safety.md) | Banners at the top; start-up refusal |

Every journey ends with the library check from SKILL.md (identical
fingerprint before and after).
