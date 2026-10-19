# Linux, Windows, and macOS

The ingest tools run on Linux, Windows, and macOS. Originals stay bit-exact on each. A source is opened read-only. A destination inside that source is refused.

## Paths

`albumen platform-dirs` prints two directories and creates nothing:

- Linux: `$XDG_CONFIG_HOME/albumen` and `$XDG_DATA_HOME/albumen`, or `~/.config/albumen` and `~/.local/share/albumen`.
- macOS: `~/Library/Application Support/albumen` for both config and data, which is the Application Support layout.
- Windows: the roaming Known Folder, under `%APPDATA%\Albumen\albumen`.

`--from` and `--to` stay required. Pass a printed path when you want the library there.

Windows paths longer than 260 characters are opened with the `\\?\` prefix, including `\\?\UNC\` for a UNC path. Reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, with or without an extension) and names that end in a dot or a space use that prefix too, so the file stored is the file you named.

When a destination directory already contains the same name in another case, the copy stops and names both spellings. The existing file stays.

Named NTFS alternate data streams are listed and copied to the destination under the same stream name. The default `::$DATA` stream is the file itself. A stream is not dropped on the way through. The source stream is only read.

NTFS stores modification time in 100 ns ticks. A time Albumen sets is kept to that tick. The copy's stored time is the time the filesystem kept.

## Credentials

`albumen-receive` still takes `--password` or `ALBUMEN_RECEIVE_PASSWORD`. Both stay out of the process output: a plan prints `password: set` or `password: unset`.

`--keyring` reads the password from the native store instead:

- macOS Keychain
- Windows Credential Manager
- Linux Secret Service

The entry name is `albumen-receive` and the account is `--user`. `--keyring-set` with `--apply` stores the password from `--password` or the environment and prints `keyring: stored`. It does not print the password. Without `--apply`, the keyring is left unchanged.

## Receiver service

`--print-service` prints a definition and writes nothing:

- Linux: a systemd user unit, `albumen-receive.service`
- macOS: a launchd agent, `io.albumen.receive.plist`, with `RunAtLoad` false
- Windows: a Task Scheduler definition, `albumen-receive.xml`, with the task disabled

`--service-dir DIR` with `--apply` writes that file into `DIR` and does not start it. The file runs `albumen-receive` with `--apply` and tells the supervisor about `ALBUMEN_RECEIVE_PASSWORD`. The password is not in the file.

Load it yourself when you want it running:

```bash
# Linux, after writing the unit into the user directory
systemctl --user daemon-reload
systemctl --user enable --now albumen-receive.service

# macOS, after copying the plist into ~/Library/LaunchAgents
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.albumen.receive.plist
```

On Windows, import `albumen-receive.xml` with Task Scheduler and enable the task. Set `ALBUMEN_RECEIVE_PASSWORD` in the user environment first.

## Tests

Pull requests run the Rust suite on Ubuntu, Windows, and macOS. The ladder, including the Windows Sandbox rung, is [test-ladder.md](test-ladder.md).
