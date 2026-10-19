# Roadmap

**First principle: metadata and original quality are never lost.**

Albumen is early. This list is the work that is not in the tree yet, plus the importers that close the gap between a read-only archive and a storage backend. Nothing here is a date.

## Safety and read-only sources

Shipped. A source is opened read-only. Scan, import, and export read through that root. A destination that resolves inside a source, or is the same path, is refused. There is no override. Export, Takeout import, and iCloud import write nothing unless `--apply` or `--execute` is passed. See [DESIGN_PHILOSOPHY.md](DESIGN_PHILOSOPHY.md), [docs/test-ladder.md](docs/test-ladder.md), and [docs/threat-model.md](docs/threat-model.md).

## Google Takeout and iCloud export

This is the gap after the safety work and before the storage backends. Both importers are in the tree.

`albumen import-takeout` reads split zip and tgz parts, or an extracted Takeout folder, read-only. Each file is `copy_verify`'d. Takeout JSON, including truncated names, `(1)` suffixes, and `.supplemental-metadata.json`, stays byte-for-byte beside the photo it describes. Capture time and GPS are restored into a new `.albumen.json` sidecar. The original bytes are not modified. A still and a video that share a stem are reported as a Motion Photo pair. The JSONL report lists every unmatched file. Nothing is dropped because of its extension. The default is a dry-run. See [docs/importers/takeout.md](docs/importers/takeout.md).

`albumen import-icloud` reads a multi-part download (zip parts plus Photo Details CSVs), a Photos export, or a copied Photos library, read-only. Originals, edited versions, Live Photo pairs, and AAE/XMP sidecars are `copy_verify`'d. CSVs stay byte-for-byte. Album, favorite, caption, capture time, and GPS metadata is a sidecar export. `Photos.sqlite` is opened `mode=ro&immutable=1`. The report lists every unmatched sidecar. The default is a dry-run. See [docs/importers/icloud.md](docs/importers/icloud.md).

Synthetic fixtures cover both imports. The throughput bench is 100,000 generated items with a budget of 4 hours. It is [`.github/workflows/takeout-scale.yml`](.github/workflows/takeout-scale.yml), on `workflow_dispatch` and a weekly schedule, not on every pull request.

## Export everything

Shipped as `albumen export` and `albumen verify-manifest`. The command copy-verifies the library, sidecars, and pairs to a directory or `s3://` prefix, writes a SHA-256 manifest, and can be run again without overwriting. See the README.

## Camera-roll background backup spike

`mobile/ios` is the host app and the background upload extension. `mobile/android` is the camera-roll library and host. Each one exports the camera original, uploads it to `albumen-receive`, and accepts the copy only when the client's SHA-256 matches `X-Albumen-Sha256` from `copy_verify`. Nothing in the camera roll is deleted. No transcode on the way in.

### iOS

- `mobile/ios` reads PhotoKit `PHAssetResource` originals. A Live Photo is the still plus the paired video. An edited asset prefers the full-size unadjusted resource when PhotoKit has one. An iCloud-optimized asset is downloaded with `isNetworkAccessAllowed`. If only a derivative is available, the export fails and logs that refusal. Adjustment data is kept as `{filename}.adjustment`. `PHImageManager.requestImage` is not used.
- The package builds with `swift test` on a macOS runner. PhotoKit is present there.
- The receiver password is stored in the Keychain. The host and the background upload extension share it through a keychain access group. The PUT request does not carry it.
- The Xcode host and the `AlbumenBackgroundUpload` extension are in `mobile/ios/Albumen.xcodeproj`. The host requests PhotoKit access and copies `PHAssetResource` originals, so location stays in the file that is uploaded. A background `URLSession` and a background-processing task continue the PUT after the app leaves the foreground. SHA-256 is checked against `X-Albumen-Sha256`. Failures are written to the shared status file and posted as a local notification. Nothing in Photos is deleted. See [docs/mobile-ios-backup.md](docs/mobile-ios-backup.md).
- CI builds that Xcode project on `macos-14` only, with code signing off. A signing identity is not in this repository, and the app is not published.

### Android

- `mobile/android` reads images and video through MediaStore with `setRequireOriginal` and `openFileDescriptor`. `requiredPermissions` is the media permissions on API 33 and newer, and `READ_EXTERNAL_STORAGE` on API 30 through 32, plus `ACCESS_MEDIA_LOCATION`. The library does not show the dialog. Without location permission, MediaStore removes GPS.
- `CameraRollBackupWorker` schedules the backup with WorkManager while the device is idle and charging, so a killed process resumes the same queue.
- A motion photo is one JPEG with the video still appended. The worker copies that file whole.
- The receiver password is in Keystore-backed storage. WorkManager input does not contain it.
- `mobile/android/host` requests those permissions and enqueues only when images, video, and location are all granted. A location denial is explained as stripped GPS and is not a finished backup. CI compiles the app and unit-tests that decision. It does not click the system dialog. Android 14 selected-photos (`READ_MEDIA_VISUAL_USER_SELECTED`) is still not a grant. The flow is [docs/camera-roll-scope.md](docs/camera-roll-scope.md).

## Connectors

Storage adapters beyond the local directory and the S3 API that exist now:

- rclone for SMB. SFTP, WebDAV, and Backblaze B2 already go through `RcloneStore`: the adapter calls `copy_verify`, and rclone is the transport. CI serves SFTP and WebDAV with rclone and uses MinIO as the B2 stand-in.
- Camera-to-cloud: `albumen-receive` accepts WebDAV, SFTP, and FTP and stores each file with `copy_verify`. S3 upload from a camera, and tethered USB import, are still open. Described in [docs/architecture.md](docs/architecture.md).
- Synced Google Drive and Dropbox folders stay a `LocalStore` root. No vendor API that rewrites the file on the way through.

Each connector refuses to overwrite, preserves the filename and mtime, and reports done only after the SHA-256 check.
