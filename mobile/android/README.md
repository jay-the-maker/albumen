# Android camera-roll skeleton

`backup` is the camera-roll library. `host` is the application that requests its permissions. `CameraRoll` opens each MediaStore image and video with `MediaStore.setRequireOriginal` and `ContentResolver.openFileDescriptor`. That is the original file, including a motion photo's embedded video. `requiredPermissions` is `READ_MEDIA_IMAGES` and `READ_MEDIA_VIDEO` on API 33 and newer, and `READ_EXTERNAL_STORAGE` on API 30 through 32, plus `ACCESS_MEDIA_LOCATION` in both cases. The library has no Activity. `HostActivity` requests the set. If images or video are denied, it does not enqueue. If location is denied, it says MediaStore would strip GPS and does not treat the backup as finished. The receiver password is not part of the permission request. CI does not click the system dialog. The runtime flow is [docs/camera-roll-scope.md](../../docs/camera-roll-scope.md).

`CameraRollBackupWorker` is a WorkManager worker. It runs while the device is idle, charging, and on a network. The receiver password is written with `EncryptedSharedPreferences` under a Keystore key. WorkManager input has the endpoint and the user, and a unit test checks that the password string is absent from that `Data`. The upload sends `X-Albumen-Mtime` from the file's last-modified time and counts only when `X-Albumen-Sha256` matches the SHA-256 of the bytes copied into the app cache. The worker deletes that cache file. It does not delete the MediaStore row.

```bash
scripts/android-ci.sh
```

The script installs command-line tools `13114758` and Gradle `8.11.1` after checking their SHA-256 sums, then runs `:backup:testDebugUnitTest`, `:host:testDebugUnitTest`, and `:host:assembleDebug`.
