# Camera-roll scope

The iOS and Android trees are libraries, not apps. This note is the work those libraries do not contain yet, and the failure already implemented when PhotoKit has only a derivative.

## iCloud originals

`OriginalPolicy` chooses the upload. `fullSizePhoto`, `adjustmentBasePhoto`, `fullSizeVideo`, `adjustmentBaseVideo`, and `fullSizePairedVideo` are originals. `.photo`, `.video`, and `.pairedVideo` are originals only when that type is the only one present and the asset is local. An iCloud-optimized asset that offers only one of those three is `UploadFailure.derivativeOnly`. `OriginalResources` logs `refusing a derivative; the full iCloud original is not available` and does not upload the derivative.

`OriginalResources.write` sets `isNetworkAccessAllowed = true` and leaves it on, so a full-size resource that is not on the device is fetched. The call does not switch to `PHImageManager.requestImage`.

`PHAsset.sourceType` marks a cloud-shared asset as remote. PhotoKit's `locallyAvailable` key, when the resource responds to it, marks an optimized original that still sits in the user library. A resource that does not respond to that key is treated as local, so a normal library photo is not refused.

## iOS background upload

New photos and videos should leave the device while Albumen is not in the foreground. That is not in `mobile/ios`.

The package has no application target and no extension target. CI runs `swift test --package-path mobile/ios` on `macos-14`. An extension needs an Xcode project, a bundle identifier, a signing identity, and a provisioning profile. Those are not in this repository.

The extension, once it exists, should:

- Share an app group with the host for the queue of asset identifiers and the last library change it handled.
- Read the receiver password from the Keychain in a keychain access group shared with the host. The password is not copied into the extension's `Info.plist` or into a file.
- Export with `OriginalResources` and `OriginalPolicy`. `isNetworkAccessAllowed` stays true. A `derivativeOnly` failure is logged, left in the queue, and not replaced with a preview upload. The Photos asset is not deleted.
- Use a background `URLSession`, or the iOS background-processing task that can run after the app is suspended, for the WebDAV PUT. The upload still counts only when `X-Albumen-Sha256` matches the file that was sent, and the client still sends `X-Albumen-Mtime`.
- Observe the photo library for new assets. The Swift package cannot register that observer without an app target.

## Android runtime permissions

`CameraRoll.requiredPermissions(sdkInt)` is the set the host requests. The library has no `Activity`, so it does not call `ActivityResultContracts.RequestMultiplePermissions` itself. `CameraRollBackupWorker` does not prompt. The host requests permission, then calls `CameraRollBackupWorker.enqueue`.

| API | Request |
| --- | --- |
| 33 and newer | `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, `ACCESS_MEDIA_LOCATION` |
| 30 through 32 | `READ_EXTERNAL_STORAGE`, `ACCESS_MEDIA_LOCATION` |

The manifest already caps `READ_EXTERNAL_STORAGE` at `maxSdkVersion` 32. A unit test checks the split.

If images or video are denied, the host does not enqueue the worker. If media is granted and location is denied, MediaStore strips GPS from the file it returns. That copy is not a complete original. The host should say so and ask for `ACCESS_MEDIA_LOCATION` again before treating the backup as finished. The worker does not delete the MediaStore row in either case.

If a grant is revoked later, the worker's MediaStore query fails or returns nothing and the worker retries. It still does not show a dialog.

Android 14's selected-photos grant (`READ_MEDIA_VISUAL_USER_SELECTED`) is not handled. The library does not open the photo picker.

The receiver password stays in Keystore-backed storage. The permission flow does not put it in the permission request or in WorkManager input.
