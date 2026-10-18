# Roadmap

**First principle: metadata and original quality are never lost.**

Albumen is early. This list is the work that is not in the tree yet. The ingest crate, the fixture gate, and the charter are what exist today. Nothing here is a date.

## Export everything

One command copies the whole library to a destination the user names: every original, every sidecar, every non-destructive edit file, and the metadata that lives inside those files. The destination can be a directory or any `BlobStore` the ingest core already speaks.

The command is `copy_verify` across the library, not an archive format that recompresses. It writes a manifest next to the export. Each line is the preserved filename, the byte length, and the SHA-256. A later check of that manifest must fail if a byte moved. The export does not delete the library it copied from.

## Camera-roll background backup spike

The iOS and Android apps are not started. The first slice is a spike that backs up the camera roll in the background and proves the copy still goes through `copy_verify` (or the same checks, if the platform forces the upload to happen inside an extension). No transcode on the way in. The camera-roll file stays until the library copy has verified.

### iOS

- Read the library with PhotoKit.
- Target the iOS 26.1 background upload extension so new photos and videos leave the device without the app staying in the foreground.
- Preserve the original resource PhotoKit returns, including Live Photo pairs, not a rendered preview.
- Keep location metadata. Do not request a version that strips GPS.

### Android

- Read images and video through MediaStore.
- Schedule the backup with WorkManager so it runs when the device is idle and on power, and so a killed process resumes the same queue.
- Hold `ACCESS_MEDIA_LOCATION`. Without it, MediaStore hands back files whose GPS has been removed, and that would break the first principle.
- Motion photos are one JPEG with the video still appended. Copy that file whole.

## Connectors

Storage adapters beyond the local directory and the S3 API that exist now:

- rclone for SMB, SFTP, WebDAV, and Backblaze B2. The adapter calls `copy_verify`; rclone is the transport.
- Camera-to-cloud endpoints: FTP, SFTP, WebDAV, and S3, for a body that uploads as it shoots, plus tethered USB import. RAW and video included. Described in [docs/architecture.md](docs/architecture.md).
- Synced Google Drive and Dropbox folders stay a `LocalStore` root. No vendor API that rewrites the file on the way through.

Each connector refuses to overwrite, preserves the filename and mtime, and reports done only after the SHA-256 check.
