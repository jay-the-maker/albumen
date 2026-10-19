# iOS background upload

The host is `mobile/ios/Albumen.xcodeproj`. The embedded extension is `AlbumenBackgroundUpload`. Both link the Swift package in `mobile/ios`. CI builds them on a Mac. The app is not published to a store, and this repository has no signing identity.

There is no Albumen account. The receiver is the address you type.

## What is copied

The host requests PhotoKit read/write access so it can read the library. It does not add photos and it does not delete them. `OriginalExportSession` writes each `PHAssetResource` original with `isNetworkAccessAllowed` left on. That file is the upload. GPS and the rest of the embedded metadata stay in those bytes. The SHA-256 of that file is what `X-Albumen-Sha256` has to match.

`OriginalPolicy` still prefers `fullSizePhoto`, `fullSizeVideo`, and `fullSizePairedVideo`. A derivative-only iCloud asset stays in the queue, is named on the status screen, and is not replaced with a preview. The next time you tap **Save and back up**, that asset is tried again.

The receiver password is saved in the Keychain and then cleared from the field. The queue file stores the endpoint and the user. It does not store the password. The background PUT has no Authorization header. When `albumen-receive` answers 401 with a Basic challenge, the session delegate reads the Keychain and supplies the credential for that challenge only (`URLCredential` persistence `.none`).

`X-Albumen-Mtime` is the asset's modification date, or its creation date when PhotoKit has no modification date, in Unix seconds.

## After you leave the app

The host creates a background `URLSession` with identifier `photos.albumen.backup.background-upload` and `sessionSendsLaunchEvents` set. iOS can finish that PUT after the app is suspended and then relaunch the host through `handleEventsForBackgroundURLSession`. The extension reconnects the same identifier when it is launched.

The host also submits a `BGProcessingTask` named `photos.albumen.backup.refresh` that requires a network and does not require power. A new photo library change queues asset identifiers created after the last one already seen and runs another pass. Identifiers already in flight are not exported again. Scratch files live in the app group and are removed after the attempt. A path outside that scratch directory is refused.

The app group is `group.photos.albumen`. The keychain access group suffix is `photos.albumen.shared`, prefixed by `$(AppIdentifierPrefix)` in the entitlements and in `AlbumenKeychainAccessGroup`. An unsigned build leaves that prefix unexpanded, so the process uses its own keychain item until a signed build shares the group.

Cleartext HTTP is allowed because the documented receiver speaks HTTP. An `https://` endpoint still uses the system trust store.

## Failures

The status screen reads `albumen-backup-status.txt`. The line is always present:

```text
Verified N this run. Already verified M. Failures K.
```

Each failure is a line above that summary, and the same text is posted as a local notification when there is at least one failure. Denying notifications leaves the status file. A missing receiver, a missing password, Photos access that is off, a SHA-256 mismatch (both hashes are named), a missing `X-Albumen-Sha256`, a refused PUT, a transport error, and an early stop of the background pass are all written there. The backup does not read Photos until the receiver and the password are present.

A limited PhotoKit selection backs up the photos you selected, still as originals.

## Build

```bash
swift test --package-path mobile/ios
scripts/ios-xcode.sh
```

`scripts/ios-xcode.sh` runs `xcodebuild` for generic iOS with `CODE_SIGNING_ALLOWED=NO`. CI runs that script on `macos-14` and does not run it on Ubuntu. `swift test` covers the PUT shape, the SHA-256 verdict, the status line, the queue, scratch deletion, and a source scan that rejects Photos deletion and preview rendering.
