# Android camera-roll skeleton

Kotlin library for a backup that is not an app yet. `CameraRoll` opens each MediaStore image and video with `MediaStore.setRequireOriginal` and `ContentResolver.openFileDescriptor`. That is the original file, including a motion photo's embedded video. The manifest requests `READ_MEDIA_IMAGES`, `READ_MEDIA_VIDEO`, and `ACCESS_MEDIA_LOCATION`. The host app still has to grant those at runtime. Without location permission, MediaStore removes GPS.

`CameraRollBackupWorker` is a WorkManager worker. It runs while the device is idle, charging, and on a network. The upload counts only when `X-Albumen-Sha256` matches the SHA-256 of the bytes copied into the app cache. The worker deletes that cache file. It does not delete the MediaStore row.

```bash
scripts/android-ci.sh
```

The script installs command-line tools `13114758` and Gradle `8.11.1` after checking their SHA-256 sums, then runs `:backup:testDebugUnitTest`.
