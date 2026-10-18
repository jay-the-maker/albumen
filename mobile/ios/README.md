# iOS camera-roll skeleton

Swift package for a backup that is not an app yet. `CameraRollBackup` asks PhotoKit for `PHAssetResource` originals and writes those bytes out. A Live Photo contributes its still and its paired video. An edited asset prefers `fullSizePhoto` or `fullSizePairedVideo` when PhotoKit has them. Adjustment data is uploaded beside the original as `{filename}.adjustment`. `PHImageManager.requestImage` is not used.

`WebDavUploader` PUTs the file to `albumen-receive` and sends `X-Albumen-Mtime` from the file's modification date. The upload counts only when `X-Albumen-Sha256` matches the SHA-256 of the file that was sent. The Photos asset is not deleted.

There is no background-upload extension target here. That needs an Xcode app and a signing identity. CI runs `swift test --package-path mobile/ios` on `macos-14`.

```bash
swift test --package-path mobile/ios
```
