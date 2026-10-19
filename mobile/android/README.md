# Android camera-roll backup

`backup` is the camera-roll library. `host` is the app that requests permission, shows backup status, and enqueues the worker. The path is [docs/mobile-android-backup.md](../../docs/mobile-android-backup.md).

`CameraRoll` opens each MediaStore image and video with `MediaStore.setRequireOriginal` and `ContentResolver.openFileDescriptor` in mode `"r"`. That is the original, including a motion photo's embedded video and its GPS, when the host holds `ACCESS_MEDIA_LOCATION`. If location is denied, the backup does not read the roll. The library has no Activity. `HostActivity` requests the permissions, enqueues only when images, video, and location are granted, and shows every failure the worker records. The receiver password is stored in the Keystore and is not part of the permission request or the WorkManager input.

The first WorkManager job runs on a network and is aimed at a verified copy within 5 minutes of setup. Later jobs run while the phone is idle, charging, and on a network. An upload counts when `X-Albumen-Sha256` matches the SHA-256 of the bytes copied into the app cache. A mismatch is retried. The worker deletes that cache file. It does not delete or update the MediaStore row. The debug APK is not published to a store.

```bash
scripts/android-ci.sh
```

The script installs command-line tools `13114758` and Gradle `8.11.1` after checking their SHA-256 sums, then runs `:backup:testDebugUnitTest`, `:host:testDebugUnitTest`, and `:host:assembleDebug`. The emulator job is `scripts/android-emulator-ci.sh`.
