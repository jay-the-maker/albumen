# Camera-roll scope

The iOS tree has a host app and a background upload extension. This note is the work still open, and the failure already implemented when PhotoKit has only a derivative.

## iCloud originals

`OriginalPolicy` chooses the upload. `fullSizePhoto`, `adjustmentBasePhoto`, `fullSizeVideo`, `adjustmentBaseVideo`, and `fullSizePairedVideo` are originals. `.photo`, `.video`, and `.pairedVideo` are originals only when that type is the only one present and the asset is local. An iCloud-optimized asset that offers only one of those three is `UploadFailure.derivativeOnly`. `OriginalResources` logs `refusing a derivative; the full iCloud original is not available` and does not upload the derivative.

`OriginalResources.write` sets `isNetworkAccessAllowed = true` and leaves it on, so a full-size resource that is not on the device is fetched. The call does not switch to `PHImageManager.requestImage`.

`PHAsset.sourceType` marks a cloud-shared asset as remote. PhotoKit's `locallyAvailable` key, when the resource responds to it, marks an optimized original that still sits in the user library. A resource that does not respond to that key is treated as local, so a normal library photo is not refused.

## iOS background upload

`mobile/ios/Albumen.xcodeproj` is the host app and the `AlbumenBackgroundUpload` extension. CI builds both on `macos-14` with `scripts/ios-xcode.sh`. The job does not sign the app and does not publish it. A signing identity is not in this repository. The path is [mobile-ios-backup.md](mobile-ios-backup.md).

The host and the extension share the app group `group.photos.albumen`. That group holds the queue of asset identifiers, the newest creation time already queued, and the status file. The receiver password is in the Keychain. When the build expands `AlbumenKeychainAccessGroup`, both targets use that access group. The password is not written into either `Info.plist`, the queue, or the PUT request. The session delegate answers the receiver's Basic challenge from the Keychain.

Export uses `OriginalResources` and `OriginalPolicy`. `isNetworkAccessAllowed` stays true. A `derivativeOnly` failure is logged, left in the queue, shown on the status screen, and not replaced with a preview upload. The Photos asset is not deleted. The upload uses a background `URLSession` (`photos.albumen.backup.background-upload`) and a `BGProcessingTask` (`photos.albumen.backup.refresh`). It counts only when `X-Albumen-Sha256` matches the file that was sent. The client sends `X-Albumen-Mtime`. The host observes the photo library and runs another pass for new assets. Every failure is a line in `albumen-backup-status.txt` and a local notification.

## Android runtime permissions

`CameraRoll.requiredPermissions(sdkInt)` is the set the host requests. The library has no `Activity`, so it does not call `ActivityResultContracts.RequestMultiplePermissions` itself. `CameraRollBackupWorker` does not prompt. `mobile/android/host` is the host app: `HostActivity` launches that contract with `requiredPermissions()`, and `PermissionGate` decides what happens next. The host requests permission, then calls `CameraRollBackupWorker.enqueue` only when the gate allows it. The backup itself is [mobile-android-backup.md](mobile-android-backup.md).

| API | Request |
| --- | --- |
| 33 and newer | `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, `ACCESS_MEDIA_LOCATION` |
| 30 through 32 | `READ_EXTERNAL_STORAGE`, `ACCESS_MEDIA_LOCATION` |

The manifest already caps `READ_EXTERNAL_STORAGE` at `maxSdkVersion` 32. A unit test checks the split.

If images or video are denied, the host does not enqueue the worker. If media is granted and location is denied, MediaStore would strip GPS, so that copy is not a complete original. The host says so, does not enqueue, and does not treat the backup as finished. The worker checks the same permissions again before it reads. A missing location grant is written to the status file and posted as a notification. The worker does not delete or update the MediaStore row.

CI compiles the host and unit-tests the gate. It does not click the system permission dialog. The emulator job grants the permissions with `GrantPermissionRule` and checks a synthetic JPEG.

If a grant is revoked later, the worker stops before the next read and says why. It still does not show a dialog. A checksum mismatch, a refused PUT, and a read error are written to the same status file. An empty pass is not a blank screen.

Android 14's selected-photos grant (`READ_MEDIA_VISUAL_USER_SELECTED`) is not handled. The library does not open the photo picker.

The receiver password stays in Keystore-backed storage. The permission flow does not put it in the permission request or in WorkManager input.
