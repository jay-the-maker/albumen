# Scheduled backup

`albumen backup --from LIBRARY --to DEST` is one repeatable run:

1. an incremental scan of LIBRARY (only new or changed files are hashed; the
   scan is kept in a state folder outside the library, by default this
   library's ledger folder, or `--state DIR`);
2. a resumable export into DEST, writing `DEST/ALBUMEN-MANIFEST.txt`;
3. a check of every item at DEST against what was exported;
4. a new dated proof pair, `DEST/ALBUMEN-PROOFS/backup-<UTC>.md` and `.json`.

A second run with nothing changed copies 0 files and writes a proof with the
same file count and combined hash. A run that is killed (power loss, sleep,
`kill -9`) is finished by the next run. LIBRARY is only read. DEST must be a
local or mounted folder; for S3 or rclone use `albumen export`.

Exit code 0 means verified. `albumen status DEST` reports the time of the
newest verified proof and prints its path.

Albumen does not run in the background. Use your system's scheduler.

## cron (Linux, macOS)

```cron
# every day at 02:30
30 2 * * * /usr/local/bin/albumen backup --from "$HOME/Pictures" --to /mnt/photo-backup >> "$HOME/.albumen-backup.log" 2>&1
```

## systemd timer (Linux)

`~/.config/systemd/user/albumen-backup.service`:

```ini
[Unit]
Description=Albumen photo backup

[Service]
Type=oneshot
ExecStart=/usr/local/bin/albumen backup --from %h/Pictures --to /mnt/photo-backup
```

`~/.config/systemd/user/albumen-backup.timer`:

```ini
[Unit]
Description=Daily Albumen photo backup

[Timer]
OnCalendar=*-*-* 02:30
Persistent=true

[Install]
WantedBy=timers.target
```

Enable with `systemctl --user enable --now albumen-backup.timer`.
`Persistent=true` runs a missed backup after the machine wakes.

## launchd (macOS)

`~/Library/LaunchAgents/org.albumen.backup.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>org.albumen.backup</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/albumen</string>
    <string>backup</string>
    <string>--from</string><string>/Users/you/Pictures</string>
    <string>--to</string><string>/Volumes/PhotoBackup</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict><key>Hour</key><integer>2</integer><key>Minute</key><integer>30</integer></dict>
  <key>StandardOutPath</key><string>/Users/you/Library/Logs/albumen-backup.log</string>
  <key>StandardErrorPath</key><string>/Users/you/Library/Logs/albumen-backup.log</string>
</dict>
</plist>
```

Load with `launchctl load ~/Library/LaunchAgents/org.albumen.backup.plist`.
launchd runs a missed interval when the Mac wakes. Reading a Photos library
may need Full Disk Access for the albumen binary.
