# Friction

User friction is minimal. The target is a handful of steps from install to a first backup: five steps or fewer, one user action each.

Sane defaults, no account, and no nags are hard rules. The pull request template asks for them. `scripts/check-philosophy-ack.py` fails the pull request unless those boxes are checked.

## Steps today

`albumen import-takeout` copies a Google Takeout download. `albumen import-icloud` copies an iCloud Photos download, export, or Photos library copy. Neither command modifies the source. The first camera backup that exists is `albumen-receive`. It stores a file only after `copy_verify`. It does not delete the source you uploaded.

With Rust 1.94 or newer already installed (CI uses 1.99.1), the path is five steps:

1. Clone the repository and change into it: `git clone https://github.com/jay-the-maker/albumen.git && cd albumen`
2. `cargo build --release --locked`
3. `mkdir library`
4. Start the receiver: `ALBUMEN_RECEIVE_PASSWORD='a receiver password' ./target/release/albumen-receive --to ./library --user camera --webdav 127.0.0.1:8080 --apply`
5. Put one original: `curl -T ./photo.jpg -u camera:'a receiver password' http://127.0.0.1:8080/photo.jpg`

The password in that command is one you chose for this receiver. It is not an Albumen account. The file on your disk stays where it was.

A machine without Rust uses the release archive. `scripts/quickstart.json` is the machine-readable list, and `scripts/check-quickstart.py` fails when it has more than five steps. The path is four steps:

1. Install: `curl -fsSL https://raw.githubusercontent.com/jay-the-maker/albumen/main/scripts/install.sh | sh` (Windows: `irm https://raw.githubusercontent.com/jay-the-maker/albumen/main/scripts/install.ps1 | iex`). The script checks the archive SHA-256 and, when the tool is on `PATH`, a cosign bundle or a GitHub attestation. The binaries go in a directory you own. There is no sudo and no account.
2. Start the receiver: `ALBUMEN_RECEIVE_PASSWORD='a receiver password' "$HOME/.local/share/albumen/bin/albumen-receive" --to "$HOME/albumen-library" --user camera --webdav 127.0.0.1:8080 --apply`
3. PUT one original the way a phone does, with `X-Albumen-Mtime`: `curl -sS -D - -o /dev/null -T "$HOME/originals/IMG_QUICKSTART.jpg" -u "camera:${ALBUMEN_RECEIVE_PASSWORD}" -H "Content-Type: application/octet-stream" -H "X-Albumen-Mtime: $(stat -c %Y "$HOME/originals/IMG_QUICKSTART.jpg" 2>/dev/null || stat -f %m "$HOME/originals/IMG_QUICKSTART.jpg")" http://127.0.0.1:8080/IMG_QUICKSTART.jpg`
4. Require the response `X-Albumen-Sha256`, the stored file, and the source to be the same SHA-256: `sha256sum "$HOME/originals/IMG_QUICKSTART.jpg" "$HOME/albumen-library/IMG_QUICKSTART.jpg" || shasum -a 256 "$HOME/originals/IMG_QUICKSTART.jpg" "$HOME/albumen-library/IMG_QUICKSTART.jpg"`. The source bytes and mtime stay as they were.

Windows commands for the same four steps are the `windows` fields in `scripts/quickstart.json`. The receiver password is still one you chose. It is not an Albumen account.

## Phone backup

The Android host sends to a receiver, so the quickstart above comes first. The phone then takes five steps. There is no account screen. The debug APK is installed by you. It is not a store install. The detail is [mobile-android-backup.md](mobile-android-backup.md).

1. Build the host with `scripts/android-ci.sh`.
2. Install the host debug APK on the device.
3. Launch `HostActivity` with intent extras `photos.albumen.host.ENDPOINT`, `photos.albumen.host.USER`, and `photos.albumen.host.PASSWORD`.
4. Grant images, video, and location. That is one system dialog. Denying images or video does not enqueue. Denying location does not start the backup, because MediaStore would strip GPS. A partial grant is not enough.
5. Leave the phone on a network.

The first job is queued with no initial delay and a network constraint, so a verified copy can finish within 5 minutes of setup. Later backups run while the phone is idle, charging, and on a network. The upload does not delete the camera-roll item. iOS background upload is still the scoped extension in [camera-roll-scope.md](camera-roll-scope.md).

## Hard rules

### Sane defaults

- `search` and `browse` are on. `export` and `upload` are off unless that process is started with `--enable-export` or `--enable-upload`.
- There is no delete tool. The CLI has no move.
- A backup that would strip GPS does not start, and the message says why.
- The receiver does not listen until you pass `--webdav`, `--sftp`, or `--ftp`.
- The Android periodic worker does not run unless the device is idle, charging, and on a network. The first job after setup runs on a network so a verified copy can finish within 5 minutes.

### No account required

There is no Albumen account, no signup, and no cloud login. The receiver user and password are the ones you pass on the command line.

A library on disk is readable and exportable with no network and no service. `albumen export --apply` copies it to a directory, and `albumen verify-manifest` checks that copy. Neither command takes an account. After the check, the export is ordinary files.

### No nags

Albumen does not nag, upsell, or ask for a rating. There is no paid tier that unlocks the original, export, or fidelity. Search does not scan the library in the background for a purpose you did not start.

The pull request checklist uses these exact lines:

```text
- [x] Sane defaults
- [x] No account required
- [x] No nags
```
