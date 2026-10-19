# Roadmap

**First principle: metadata and original quality are never lost.**

Albumen is early. This list is the work that is not in the tree yet. The ingest crate, the fixture gate, and the charter are what exist today. Nothing here is a date.

## Google Takeout

Shipped as `albumen import-takeout`. Split zip and tgz parts are streamed read-only, each original is `copy_verify`'d, and Takeout JSON stays byte-for-byte beside the original. See [docs/importers/takeout.md](docs/importers/takeout.md).

## iCloud Photos

Shipped as `albumen import-icloud`. A multi-part download (zip parts plus Photo Details CSVs), a Photos export, or a copied Photos library is read-only. Originals, edited versions, Live Photo pairs, and AAE/XMP sidecars are `copy_verify`'d. CSVs stay byte-for-byte. Album, favorite, and caption metadata is a sidecar export from `Photos.sqlite` opened `mode=ro&immutable=1`. See [docs/importers/icloud.md](docs/importers/icloud.md).

## Export everything

Shipped as `albumen export` and `albumen verify-manifest`. The command copy-verifies the library, sidecars, and pairs to a directory or `s3://` prefix, writes a SHA-256 manifest, and can be run again without overwriting. See the README.

## Camera-roll background backup spike

Skeletons are in `mobile/ios` and `mobile/android`. They are not apps. Each one exports the camera original, uploads it to `albumen-receive`, and accepts the copy only when the client's SHA-256 matches `X-Albumen-Sha256` from `copy_verify`. Nothing in the camera roll is deleted. No transcode on the way in.

### iOS

- `mobile/ios` reads PhotoKit `PHAssetResource` originals. A Live Photo is the still plus the paired video. An edited asset prefers the full-size unadjusted resource when PhotoKit has one. An iCloud-optimized asset is downloaded with `isNetworkAccessAllowed`. If only a derivative is available, the export fails and logs that refusal. Adjustment data is kept as `{filename}.adjustment`. `PHImageManager.requestImage` is not used.
- The package builds with `swift test` on a macOS runner. PhotoKit is present there.
- The receiver password is stored in the Keychain.
- Still open: an iOS 26.1 background upload extension, so new photos and videos leave the device without the app staying in the foreground. The scope is [docs/camera-roll-scope.md](docs/camera-roll-scope.md). That target needs an Xcode project and a signing identity, which this package does not have.
- Still open: a host that requests PhotoKit access without a version that strips GPS.

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
