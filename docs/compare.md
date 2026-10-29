# How Albumen compares

This page compares Albumen with iCloud Photos, Google Photos and Frame.io.
Every Albumen row points at a file or command on `main`.

- **on a branch, not released**: the work is pushed to a branch and is not merged into `main` yet.
- **not yet**: Albumen does not have the feature.

Competitor prices were fetched from each vendor's own pricing page on 2026-10-29. They are US prices before tax. Check the linked page for your country.

## Price

| | Albumen | iCloud Photos | Google Photos | Frame.io |
|---|---|---|---|---|
| Cost | Free, Apache-2.0 ([LICENSE](../LICENSE)). You supply the disk or bucket. | 5 GB free. iCloud+ 50 GB $0.99/mo, 200 GB $2.99/mo, 2 TB $9.99/mo, 6 TB $29.99/mo, 12 TB $59.99/mo ([Apple, fetched 2026-10-29](https://support.apple.com/en-us/108047)) | 15 GB free per Google account. Basic 100 GB $1.99/mo, Google AI Plus 2 TB $9.99/mo ([Google One, fetched 2026-10-29](https://one.google.com/about/plans)) | Free plan with 2 GB. Pro $15 per member per month, Team $25 per member per month, Enterprise custom ([Frame.io, fetched 2026-10-29](https://frame.io/pricing)) |
| Account needed | No. The CLI runs locally (`albumen --help`). | Apple Account | Google Account | Frame.io account. Reviewers use share links without one ([pricing FAQ](https://frame.io/pricing)) |

## Features

| Feature | Albumen | iCloud Photos | Google Photos | Frame.io |
|---|---|---|---|---|
| Bit-exact copy with SHA-256 read-back | Yes: `albumen export` ([cli/src/main.rs](../cli/src/main.rs), [ingest/src/local.rs](../ingest/src/local.rs)) | — | — | — |
| Re-check an export later | Yes: `albumen verify-manifest` ([cli/src/main.rs](../cli/src/main.rs)) | — | — | — |
| Proof file you can keep from a verify run | on a branch, not released (`verify-manifest --proof`, branch `verify-manifest-proof`) | — | — | — |
| Import from Google Takeout | Yes: `albumen import-takeout` ([docs/importers/takeout.md](importers/takeout.md)) | — | Source of the export | — |
| Compare Takeout JSON and EXIF time and GPS | on a branch, not released (`albumen takeout-fidelity`, branch `takeout-fidelity`) | — | — | — |
| Import from iCloud or a Photos library | Yes: `albumen import-icloud` ([docs/importers/icloud.md](importers/icloud.md)) | Source of the export | — | — |
| Live Photo pairs kept together | Yes ([ingest/tests/synthetic_live_photo.rs](../ingest/tests/synthetic_live_photo.rs)). Pairing by ContentIdentifier with flags is on a branch, not released (branch `icloud-fidelity`). | Yes | Yes | — |
| Read-only scan of an existing library | Yes: `albumen scan` ([cli/tests/scan_source_untouched.rs](../cli/tests/scan_source_untouched.rs)) | — | — | — |
| Local account of every file and connection | Yes: `albumen ledger` ([ingest/src/ledger.rs](../ingest/src/ledger.rs)) | — | — | — |
| Export to S3-compatible storage | Yes: `albumen export --to s3://bucket/prefix` ([ingest/src/s3.rs](../ingest/src/s3.rs)) | — | — | Storage Connect on Enterprise ([pricing](https://frame.io/pricing)) |
| Family libraries with signed manifests | Yes: `albumen family` ([docs/design/family-sharing.md](design/family-sharing.md)) | Shared Library | Partner sharing | Workspaces and members |
| Camera upload over WebDAV, SFTP and FTP | Yes: `albumen-receive` ([README.md](../README.md#camera-receiver)) | — | — | Camera to Cloud ([pricing](https://frame.io/pricing)) |
| Phone camera-roll backup | Android app and iOS library ([docs/mobile-android-backup.md](mobile-android-backup.md), [mobile/ios](../mobile/ios)) | Yes | Yes | iOS app |
| Search by date, camera, lens, GPS box and keywords | Yes: `albumen-mcp` ([mcp/src/tools.rs](../mcp/src/tools.rs)) | Yes | Yes | Metadata search |
| Agent access over MCP | Yes: `albumen-mcp` ([AGENTS.md](../AGENTS.md)) | — | — | — |
| Exit codes 0–4 and a `next:` line on errors | on a branch, not released (branch `exit-codes`) | — | — | — |
| Face or people search | not yet | Yes | Yes | — |
| Review and approval with frame-accurate comments | not yet | — | — | Yes ([pricing](https://frame.io/pricing)) |
| Hosted sync service | not yet. Albumen copies to a disk or bucket you own. | Yes | Yes | Yes |
| Web or desktop gallery app | not yet ([ROADMAP.md](../ROADMAP.md)) | Yes | Yes | Yes |

A "—" means the product does not offer this as a feature. It does not mean the product has been checked to be worse at it.
