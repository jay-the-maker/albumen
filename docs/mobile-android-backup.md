# Android camera-roll backup

The Android host copies camera-roll originals to an `albumen-receive` WebDAV endpoint. A copy counts when the client's SHA-256 matches `X-Albumen-Sha256` from the receiver. The camera-roll item stays on the phone. The host is not published to an app store. CI builds the debug APK and does not upload it.

## What is copied

`CameraRoll` lists images and videos from MediaStore, one page at a time, newest first. Each file is opened with `MediaStore.setRequireOriginal` and `ContentResolver.openFileDescriptor` in mode `"r"`. That is the original, including a motion photo's embedded video. The bytes are spooled into the app cache, hashed, and sent. The cache file is removed after the attempt. The MediaStore row is not deleted, updated, or rewritten.

GPS stays in the file when the host holds `ACCESS_MEDIA_LOCATION`. If that permission is missing, MediaStore would strip GPS, so the worker does not open the camera roll. The status text says why, and the backup is not finished.

| API | Permissions |
| --- | --- |
| 34 and newer | `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, `ACCESS_MEDIA_LOCATION`, plus `READ_MEDIA_VISUAL_USER_SELECTED` in the request so Select photos can be named |
| 33 | `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, `ACCESS_MEDIA_LOCATION` |
| 30 through 32 | `READ_EXTERNAL_STORAGE`, `ACCESS_MEDIA_LOCATION` |

`X-Albumen-Mtime` is the MediaStore `DATE_MODIFIED` in Unix seconds. The receiver records that as the file's `source_mtime` on the first store.

## WorkManager

`CameraRollBackupWorker.enqueue` schedules two jobs. The receiver password is written with `EncryptedSharedPreferences` under a Keystore key. WorkManager input has the endpoint, the user, the page size, and the pass budget. A unit test checks that the password string is absent from that input.

The first job is a one-time expedited request with a connected-network constraint and no initial delay. It does not wait for the 15-minute periodic floor, for idle, or for a charger. When expedited quota is gone, WorkManager runs it as an ordinary one-time job with the same network constraint. The target is a verified upload within 5 minutes of setup when the phone has a network and the receiver accepts the PUT. The emulator job times that path from `HostActivity` setup to the verified PUT. It does not time a physical phone.

The periodic job runs every 15 minutes, which is WorkManager's minimum, and only while the phone is idle, charging, and on a network. That is the battery default for later backups. WorkManager honors both constraint sets.

A pass reads one page (soft default 32, override `pageSize` on `enqueue`) and stops after a soft time budget (default 4 minutes, override `budgetMillis`) so the process can resume. Neither number is a cap on the camera roll. Already verified items are recorded in an app-private progress file as content URI, size, and modified time. The file does not hold pixels or GPS. Dropping it uploads again. The receiver answers `409` with the same `X-Albumen-Sha256` when those bytes are already stored, and that response counts as verified. A changed size or modified time is uploaded again. The receiver keeps a different file under a free name.

## Verify and retry

The PUT is WebDAV to the receiver root, with HTTP Basic auth. When the host is given `photos.albumen.host.MEMBER`, that value is the Ed25519 public key of a member who has joined a family library. The PUT path is `members/<that-key>/<filename>` and the endpoint has to be the library root. A path outside that prefix is refused before the request. The status file and notification 1001 name the file. `albumen-receive --member` is the receiver that stores only under that prefix. `201` and a matching `X-Albumen-Sha256` is a new object. `409` and the same digest means those bytes were already stored. A mismatch names both hashes and tries the PUT three times in that pass. Three attempts is not a cap on retries across passes. The first job then asks WorkManager to retry with exponential backoff. The periodic job cannot carry backoff while it requires idle, so the failure stays on screen and the next idle, charging, and network window continues the unfinished items. `401` and `403` stop the pass and are shown. They do not spin. There is no read timeout on the body, so a large video is not failed for its size. A receiver that does not accept a connection fails after 30 seconds, and that failure is shown.

## Failures

Every pass writes `albumen-backup-status.txt` in app-private storage and posts one notification on the Backup channel. The notification is replaced in place. The host screen shows the permission result and that file. A missing endpoint, a missing password, a missing permission, a read error, a refused PUT, a missing server hash, a checksum mismatch, a pause, and an empty camera roll each produce a non-blank line. Notification permission is requested on API 33 and is not required for the backup to run. If it is denied, the screen still has the text.

The backup does not nag, ask for a rating, or scan for a purpose other than the backup that was just started.

## Setup

The receiver comes first. With Rust installed, that path is in [friction.md](friction.md). The phone then takes the four steps in that file. There is no Albumen account.

1. Install the host debug APK from `:host:assembleDebug`. This is not a store install.
2. Open `HostActivity`.
3. Grant images, video, and location. That is one system dialog. Denying images or video does not enqueue. Denying location does not start the backup.
4. Scan the `albumen-pair://v1` QR from `albumen-receive --pair`. The scanner calls `HostActivity.onScannedPairing` with the decoded URI and the certificate fingerprint the handshake presented.

The first verified copy follows that scan. It is not another tap. The phone stays on a network. The one-time token in the URI is the receiver password. The receiver user is `camera`, the name printed beside the QR. The password is stored in the Keystore and is not put in the permission request or the WorkManager input.

The endpoint in the QR is `https://`. The documented receiver also speaks HTTP, and the host allows cleartext to the address you pass. An `https://` endpoint still uses the platform trust store. `Pairing.parse` reads the URI, and `Pairing.pin` compares the certificate SHA-256 with the fingerprint in the code. A mismatch is a sentence on the status path and the copy does not start. The emulator job feeds the decoded URI the way the scanner would, and points the upload at the same host and port over HTTP so the loopback can accept the PUT. That hook is the test. It does not rewrite a pairing code that lacks `https://`.

On Android 14 and newer, **Select photos** grants `READ_MEDIA_VISUAL_USER_SELECTED` alone. The host shows that Albumen backs up the whole camera roll and offers one **Allow all** button back to full access. The backup starts after full access is granted. Nothing is copied from a partial grant, and the worker checks again before every pass and every item. The library does not open the photo picker.

## Tests

`scripts/android-ci.sh` runs the host and backup unit tests, including Robolectric, and assembles the debug APK. The tests use a generated one-pixel JPEG with synthetic GPS EXIF (`1°2'3" N`, `4°5'6" E`). They are not photographs.

A source test reads the main Kotlin and the manifests and fails if a MediaStore delete or update, a write-mode `openFileDescriptor`, or a storage-write permission appears. `spool.file.delete()` is the cache file.

`scripts/android-soak.sh` repeats those backup cycles against a loopback receiver and prints the verified percent. Each sequence injects a closed port, a socket closed during the PUT body, a kill that stops the pass during that upload, and a SHA-256 mismatch, then runs a healthy recovery cycle. An item that was attempted and is still unverified has to be named in that cycle's status file or in notification 1001. The measured stand-in for a 24-hour verified percent, and the parts a device run still covers, are in [performance-budgets.md](performance-budgets.md).

CI runs the instrumented tests on `ubuntu-latest`. The job enables KVM with a udev rule and boots API 30 `aosp_atd` x86_64 through `reactivecircus/android-emulator-runner` v2.38.0. `scripts/android-emulator-connected.sh` then runs `:backup:connectedDebugAndroidTest` and `:host:connectedDebugAndroidTest` on the emulator the action already started. The backup test inserts the synthetic JPEG, reads it back through `setRequireOriginal`, PUTs the bytes to a loopback receiver, and checks that the received file's GPS EXIF segment is byte-identical to the source. The MediaStore bytes stay unchanged. The host test launches `HostActivity` with the receiver extras. That is setup for the five-minute and failure checks. It checks that the first verified copy is stored on a loopback receiver in under 5 minutes, and that the GPS EXIF of that copy matches the source. A second host test reads `phone-friction-steps.txt`, requires the same four steps as iOS and no more than five, opens the activity with no receiver extras, grants media and location, and feeds `onScannedPairing` the decoded `albumen-pair://v1` URI. The verified copy of the synthetic JPEG, including its GPS EXIF, has to land on the loopback, and the screen has to say the copy verified with zero failures. The camera-roll row stays. It then runs setup against a closed port and against a receiver that returns a different SHA-256. Each of those failures is a non-blank line on the status screen and the same line in notification `1001`, with a failure count above zero. The test blocks until the status file and that notification contain the failure, waking on the file rename and on WorkManager state changes. It then waits for that pass to leave `RUNNING`, because WorkManager cancels the foreground notification's ID when a pass ends. The running pass uses notification `1002`, so the failure in `1001` stays after the pass. It then resumes the activity until the status screen shows that same failure. The resume right after the permission grant can still show only "Media and location access were granted. Backup can run.", so that screen read repeats until the failure line is visible or the same deadline passes. The failure line, a failure count above zero, and the matching notification are still required. The bound is inside the test timeout. CI does not click the system permission dialog. The instrumented tests grant the permissions with `GrantPermissionRule`. The same job also boots API 34 `aosp_atd` x86_64. There, `scripts/android-emulator-selected-photos.sh` first runs `SelectedPhotosInstrumentedTest` alone on a fresh install. It grants only `READ_MEDIA_VISUAL_USER_SELECTED` and location, and checks that a pass reads nothing, sends nothing to the loopback receiver, and reports the Allow all line. It then grants full images and video access, as Allow all would, and checks that the synthetic JPEG is verified on the loopback with byte-identical GPS EXIF and the MediaStore row still there. The rest of the connected tests then run on API 34 too. `scripts/android-emulator-ci.sh` is the local path: it boots the same image when `/dev/kvm` is present, then calls the connected script.
