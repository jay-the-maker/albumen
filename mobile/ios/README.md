# iOS camera-roll skeleton

Swift package for a backup that is not an app yet. `CameraRollBackup` asks PhotoKit for `PHAssetResource` originals and writes those bytes out with `isNetworkAccessAllowed` left on, so an iCloud-optimized asset downloads the full original. A Live Photo contributes its still and its paired video. `OriginalPolicy` prefers `fullSizePhoto`, `fullSizePairedVideo`, and `fullSizeVideo`. `.photo`, `.video`, and `.pairedVideo` are accepted only when that is the only type and the asset is local. If an iCloud asset offers only a derivative, the export throws `UploadFailure.derivativeOnly` and logs the refusal. Adjustment data is uploaded beside the original as `{filename}.adjustment`. `PHImageManager.requestImage` is not used.

`WebDavUploader` PUTs the file to `albumen-receive` and sends `X-Albumen-Mtime` from the file's modification date. `WebDavUploader.fromKeychain` reads the receiver password from the Keychain. The upload counts only when `X-Albumen-Sha256` matches the SHA-256 of the file that was sent. The Photos asset is not deleted.

There is no background-upload extension target here. The scope is [docs/camera-roll-scope.md](../../docs/camera-roll-scope.md). CI runs `swift test --package-path mobile/ios` on `macos-14`.

```bash
swift test --package-path mobile/ios
```
