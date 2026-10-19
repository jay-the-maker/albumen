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

A machine without Rust has a sixth step, installing the toolchain, and that path is over the handful.

## Phone backup

The Android host and the iOS simulator count the same user-facing steps. The list is [mobile/android/host/src/androidTest/assets/phone-friction-steps.txt](../mobile/android/host/src/androidTest/assets/phone-friction-steps.txt), and the iOS copy is the same file. CI fails the job when that list is longer than five. There is no account screen. The debug build is installed by you. It is not a store install. The detail is [mobile-android-backup.md](mobile-android-backup.md).

1. Install the app.
2. Open it.
3. Grant images, video, and location. That is one system dialog. Denying images or video does not enqueue. Denying location does not start the backup, because MediaStore would strip GPS. A partial grant is not enough.
4. Scan the `albumen-pair://v1` QR from `albumen-receive --pair`. The scanner hands the app the decoded URI. The one-time token in that URI is the receiver password. The receiver user is the name printed beside the QR.

The first verified copy follows that scan. It is not another tap. The phone stays on a network. The upload does not delete the camera-roll item.

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
