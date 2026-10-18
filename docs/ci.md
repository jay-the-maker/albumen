# CI

The round-trip job does not use `ubuntu-latest`. That label starts moving to Ubuntu 26.04 on 2026-10-19. The workflow pins `ubuntu-24.04` and also runs `ubuntu-26.04`.

ExifTool is Image-ExifTool **13.59**, SHA-256 `668ea3acececb7235fbd0f4900e72d5f12c9b07e5c778fd36cb1e9b5828fd65a`, installed by `scripts/install-exiftool.sh` from the SourceForge release tarball. The 12.76 tarball that generated the fixture bytes returns 404 from exiftool.org and from SourceForge, so the gate cannot pin 12.76. Fixture bytes are not regenerated. Metadata comparisons use this one binary on both the source and the copy.

MinIO stays at `RELEASE.2025-09-07T16-13-09Z`, SHA-256 `7c5bd8512c6e966455b1d198209358b2d191c77a83ab377c4073281065fb855f`, in `scripts/with-minio.sh`. The workflow downloads that binary and runs it. The tree does not contain MinIO source.
