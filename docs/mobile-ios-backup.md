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
scripts/ios-simulator.sh
```

`scripts/ios-xcode.sh` runs `xcodebuild` for generic iOS with `CODE_SIGNING_ALLOWED=NO`. CI runs that script on `macos-14` and does not run it on Ubuntu. `swift test` covers the PUT shape, the SHA-256 verdict, the status line, the queue, scratch deletion, and a source scan that rejects Photos deletion and preview rendering.

## What the simulator job proves

`scripts/ios-simulator.sh` runs on the `macos-14` runner only. It builds `albumen-receive`, listens on `127.0.0.1:18080` with `--apply`, boots an iPhone simulator whose runtime is not newer than that Xcode's simulator SDK, and runs `AlbumenSimulatorTests` inside `AlbumenSimulatorHost`. That host is ad-hoc signed so the simulator can launch it. It allows cleartext HTTP to the loopback receiver, which is the process the notification grant uses. When `simctl privacy` cannot grant notifications, the job grants them with pinned applesimutils 0.9.12 so the permission alert does not appear. It is not the camera-roll app, and it does not embed the upload extension. The repository still has no development team.

`albumen-receive` answers `100 Continue` when a PUT asks for it and the body has not arrived yet. `URLSession` waits for that interim response before it sends the file.

Setup is saving the receiver address and the Keychain password, which is what **Save and back up** does before it copies. The clock starts there. The test then uploads a generated one-pixel JPEG, with synthetic GPS EXIF at 1°2'3" N, 4°5'6" E, through `ForegroundUpload`. That type builds `BackgroundPut.request`, answers the receiver's Basic challenge from the Keychain, and records `BackgroundOutcome`. The copy counts when `albumen-receive` returns `X-Albumen-Sha256` for those bytes. The test reads the stored file back and requires its Exif APP1 segment, marker included, to equal the source segment. The elapsed time has to be under 5 minutes.

A closed port, and a loopback that answers `201` with a different SHA-256, each write a non-blank line to `albumen-backup-status.txt`. That file is the text the status screen shows. The same text is posted with `BackupNotice`, the local notification `FailureNotice` sends. The test fails if either surface is blank, if the failure line is missing from either, or if the summary says `Failures 0.`

The mismatch loopback is there because a correct `albumen-receive` returns the digest of the bytes it stored. It cannot produce a mismatch.

## What the simulator job does not prove

The simulator does not finish a background `URLSession`. This job does not create `photos.albumen.backup.background-upload`, does not wait for `sessionSendsLaunchEvents`, and does not relaunch the camera-roll host. It also does not submit `photos.albumen.backup.refresh` or launch the `AlbumenBackgroundUpload` extension. Those stay in the device path. The generic iOS build still compiles that host and extension, unsigned. This job installs `AlbumenSimulatorHost` only.

The job does not tap **Save and back up**, and it does not present the Photos permission dialog. It calls the save and the upload the button uses, and it reads the status file the screen renders.

It does not export a `PHAssetResource`. Photos can rewrite a file it imports, so the bytes in this job are the synthetic original the export would hand to the upload. GPS in that file is the generated block above. It is not a photograph, and it is not a location from a camera roll.

The test bundle does not claim the app group or the shared keychain group. An unsigned build already falls back to the app's own container and keychain, which is the same fallback documented above. The entitlements remain in the Xcode project for a signed build. This job does not install on a phone, and it does not publish the app.
