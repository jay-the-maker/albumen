# iOS camera-roll backup

`Albumen.xcodeproj` is the host and the `AlbumenBackgroundUpload` extension. The Swift package is what they both call. The path is [docs/mobile-ios-backup.md](../../docs/mobile-ios-backup.md).

`CameraRollBackup` and `OriginalExportSession` ask PhotoKit for `PHAssetResource` originals and write those bytes out with `isNetworkAccessAllowed` left on, so an iCloud-optimized asset downloads the full original. A Live Photo contributes its still and its paired video. `OriginalPolicy` prefers `fullSizePhoto`, `fullSizePairedVideo`, and `fullSizeVideo`. `.photo`, `.video`, and `.pairedVideo` are accepted only when that is the only type and the asset is local. If an iCloud asset offers only a derivative, the export throws `UploadFailure.derivativeOnly`, the asset stays queued, and a preview is not uploaded. Adjustment data is uploaded beside the original as `{filename}.adjustment`. `PHImageManager.requestImage` is not used.

The background session PUTs the file to `albumen-receive` and sends `X-Albumen-Mtime`. The request has no Authorization header. The session delegate answers the Basic challenge from the Keychain. The upload counts only when `X-Albumen-Sha256` matches the SHA-256 of the file that was sent. The Photos asset is not deleted. Every failure is written to the status file and can be posted as a local notification.

```bash
swift test --package-path mobile/ios
scripts/ios-xcode.sh
scripts/ios-simulator.sh
```

`scripts/ios-xcode.sh` needs Xcode. CI runs it on `macos-14` only, with code signing off. `scripts/ios-simulator.sh` is the simulator job on that same runner. It launches `AlbumenSimulatorHost` so the test can reach a loopback receiver. That host is not the camera-roll app. The app is not published to a store.
