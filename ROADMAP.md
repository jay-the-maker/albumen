# Roadmap

**First principle: metadata and original quality are never lost.**

Albumen is early. This list is the work that is not in the tree yet. The ingest crate, the fixture gate, and the charter are what exist today. Nothing here is a date.

## Export everything

Shipped as `albumen export` and `albumen verify-manifest`. The command copy-verifies the library, sidecars, and pairs to a directory or `s3://` prefix, writes a SHA-256 manifest, and can be run again without overwriting. See the README.

## Camera-roll background backup spike

Skeletons are in `mobile/ios` and `mobile/android`. They are not apps. Each one exports the camera original, uploads it to `albumen-receive`, and accepts the copy only when the client's SHA-256 matches `X-Albumen-Sha256` from `copy_verify`. Nothing in the camera roll is deleted. No transcode on the way in.

### iOS

- `mobile/ios` reads PhotoKit `PHAssetResource` originals. A Live Photo is the still plus the paired video. An edited asset prefers the full-size unadjusted resource when PhotoKit has one. Adjustment data is kept as `{filename}.adjustment`. `PHImageManager.requestImage` is not used.
- The package builds with `swift test` on a macOS runner. PhotoKit is present there.
- The receiver password is stored in the Keychain.
- Still open: an iOS 26.1 background upload extension, so new photos and videos leave the device without the app staying in the foreground. That target needs an Xcode project and a signing identity, which this package does not have.
- Still open: a host that requests PhotoKit access without a version that strips GPS.

### Android

- `mobile/android` reads images and video through MediaStore with `setRequireOriginal` and `openFileDescriptor`. The manifest requests `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, and `ACCESS_MEDIA_LOCATION`. The host app still has to grant those at runtime. Without location permission, MediaStore removes GPS.
- `CameraRollBackupWorker` schedules the backup with WorkManager while the device is idle and charging, so a killed process resumes the same queue.
- A motion photo is one JPEG with the video still appended. The worker copies that file whole.
- The receiver password is in Keystore-backed storage. WorkManager input does not contain it.
- Still open: an application module and the runtime permission request.

## Connectors

Storage adapters beyond the local directory and the S3 API that exist now:

- rclone for SMB, SFTP, WebDAV, and Backblaze B2. The adapter calls `copy_verify`; rclone is the transport.
- Camera-to-cloud: `albumen-receive` accepts WebDAV, SFTP, and FTP and stores each file with `copy_verify`. S3 upload from a camera, and tethered USB import, are still open. Described in [docs/architecture.md](docs/architecture.md).
- Synced Google Drive and Dropbox folders stay a `LocalStore` root. No vendor API that rewrites the file on the way through.

Each connector refuses to overwrite, preserves the filename and mtime, and reports done only after the SHA-256 check.
