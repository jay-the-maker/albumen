# Friction

User friction is minimal. The target is a handful of steps from install to a first backup: five steps or fewer, one user action each.

Sane defaults, no account, and no nags are hard rules. The pull request template asks for them. `scripts/check-philosophy-ack.py` fails the pull request unless those boxes are checked.

## Steps today

`albumen import-takeout` copies a Google Takeout download. `albumen import-icloud` copies an iCloud Photos download, export, or Photos library copy. Neither command modifies the source. The first camera backup that exists is `albumen-receive`. It stores a file only after `copy_verify`. It does not delete the source you uploaded.

With Rust 1.94 or newer already installed (CI uses 1.99.1), the path is five steps:

1. Clone the repository and change into it: `git clone https://github.com/jay-the-maker/albumen.git && cd albumen`
2. `cargo build --release --locked`
3. `mkdir library`
4. Start the receiver: `ALBUMEN_RECEIVE_PASSWORD='a receiver password' ./target/release/albumen-receive --to ./library --user camera --webdav 127.0.0.1:8080`
5. Put one original: `curl -T ./photo.jpg -u camera:'a receiver password' http://127.0.0.1:8080/photo.jpg`

The password in that command is one you chose for this receiver. It is not an Albumen account. The file on your disk stays where it was.

A machine without Rust has a sixth step, installing the toolchain, and that path is over the handful.

## Phone backup

Phone backup is not five steps. It sends to a receiver, so the five steps above come first. The host then adds seven more, which is over the target until the apps exist:

1. Build the host with `scripts/android-ci.sh`.
2. Install the host debug APK on the device.
3. Launch `HostActivity` with intent extras `photos.albumen.host.ENDPOINT`, `photos.albumen.host.USER`, and `photos.albumen.host.PASSWORD`. There is no settings screen and no account screen.
4. Grant images, video, and location. That is one system dialog. Denying images or video does not enqueue. Denying location does not start the backup, because MediaStore would strip GPS. A partial grant is not enough.
5. Leave the device idle.
6. Put it on a charger.
7. Connect it to a network.

`CameraRollBackupWorker` runs only while all three of those are true. The upload does not delete the camera-roll item.

## Hard rules

### Sane defaults

- `search` and `browse` are on. `export` and `upload` are off unless that process is started with `--enable-export` or `--enable-upload`.
- There is no delete tool. The CLI has no move.
- A backup that would strip GPS does not start, and the message says why.
- The receiver does not listen until you pass `--webdav`, `--sftp`, or `--ftp`.
- The Android worker does not run unless the device is idle, charging, and on a network.

### No account required

There is no Albumen account, no signup, and no cloud login. The receiver user and password are the ones you pass on the command line.

A library on disk is readable and exportable with no network and no service. `albumen export` copies it to a directory, and `albumen verify-manifest` checks that copy. Neither command takes an account. After the check, the export is ordinary files.

### No nags

Albumen does not nag, upsell, or ask for a rating. There is no paid tier that unlocks the original, export, or fidelity. Search does not scan the library in the background for a purpose you did not start.

The pull request checklist uses these exact lines:

```text
- [x] Sane defaults
- [x] No account required
- [x] No nags
```
