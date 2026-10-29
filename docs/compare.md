# How Albumen compares

This page compares Albumen with iCloud Photos, Google Photos and Frame.io.
Every Albumen row points at a file or command on `main`.

- **on a branch, not released**: the work is pushed to a branch and is not merged into `main` yet.
- **not yet**: Albumen does not have the feature.

Every competitor price and feature claim comes from a vendor page fetched on 2026-10-29 and linked below. A claim no fetched page backs is marked **unverified**. Prices are US prices before tax; check the linked page for your country.

Sources, all fetched 2026-10-29:

- [A1] Apple, Set up and use iCloud Photos: https://support.apple.com/en-us/108782
- [A2] Apple, iCloud+ plans and pricing: https://support.apple.com/en-us/108047
- [G1] Google Photos Help, Back up photos & videos: https://support.google.com/photos/answer/6193313
- [G2] Google Photos Help, Set up & manage your face groups: https://support.google.com/photos/answer/6128838
- [G3] Google One plans: https://one.google.com/about/plans
- [F1] Frame.io pricing and FAQ: https://frame.io/pricing

## Price

| | Albumen | iCloud Photos | Google Photos | Frame.io |
|---|---|---|---|---|
| Cost | Free, Apache-2.0 ([LICENSE](../LICENSE)). You supply the disk or bucket. | 5 GB free. iCloud+ 50 GB $0.99/mo, 200 GB $2.99/mo, 2 TB $9.99/mo, 6 TB $29.99/mo, 12 TB $59.99/mo [A2] | 15 GB free per Google account. Basic 100 GB $1.99/mo, Google AI Plus 2 TB $9.99/mo [G3] | Free plan with 2 GB. Pro $15 per member per month, Team $25 per member per month, Enterprise custom [F1] |
| Account needed | No. The CLI runs locally (`albumen --help`). | Apple Account [A1] | Google Account [G1] | Members need an account; outside reviewers use share links without one [F1] |

## Features

| Feature | Albumen | iCloud Photos | Google Photos | Frame.io |
|---|---|---|---|---|
| Bit-exact copy with SHA-256 read-back | Yes: `albumen export` ([cli/src/main.rs](../cli/src/main.rs), [ingest/src/local.rs](../ingest/src/local.rs)) | unverified | unverified | unverified |
| Re-check an export later | Yes: `albumen verify-manifest` ([cli/src/main.rs](../cli/src/main.rs)) | unverified | unverified | unverified |
| Proof file you can keep from a verify run | on a branch, not released (`verify-manifest --proof`, branch `verify-manifest-proof`) | unverified | unverified | unverified |
| Import from Google Takeout | Yes: `albumen import-takeout` ([docs/importers/takeout.md](importers/takeout.md)) | unverified | unverified | unverified |
| Compare Takeout JSON and EXIF time and GPS | on a branch, not released (`albumen takeout-fidelity`, branch `takeout-fidelity`) | unverified | unverified | unverified |
| Import from iCloud or a Photos library | Yes: `albumen import-icloud` ([docs/importers/icloud.md](importers/icloud.md)) | Download copies of your library [A1] | unverified | unverified |
| Live Photos kept | Yes, as pairs ([ingest/tests/synthetic_live_photo.rs](../ingest/tests/synthetic_live_photo.rs)). Pairing by ContentIdentifier with flags is on a branch, not released (branch `icloud-fidelity`). | Stored in original format [A1] | Backed up from the iPhone or iPad app [G1] | unverified |
| Read-only scan of an existing library | Yes: `albumen scan` ([cli/tests/scan_source_untouched.rs](../cli/tests/scan_source_untouched.rs)) | unverified | unverified | unverified |
| Local account of every file and connection | Yes: `albumen ledger` ([ingest/src/ledger.rs](../ingest/src/ledger.rs)) | unverified | unverified | unverified |
| Copy to your own S3 bucket | Yes: `albumen export --to s3://bucket/prefix` ([ingest/src/s3.rs](../ingest/src/s3.rs)) | unverified | unverified | Storage Connect, Enterprise Prime only [F1] |
| Shared family or team library | Yes: `albumen family` ([docs/design/family-sharing.md](design/family-sharing.md)) | unverified | unverified | Members, projects and workspaces [F1] |
| Camera upload | `albumen-receive` over WebDAV, SFTP and FTP ([README.md](../README.md#camera-receiver)) | unverified | unverified | Camera to Cloud [F1] |
| Phone camera-roll backup | Android app and iOS library ([docs/mobile-android-backup.md](mobile-android-backup.md), [mobile/ios](../mobile/ios)) | Automatic upload [A1] | Automatic backup [G1] | unverified |
| Metadata search | By date, camera, lens, GPS box and keywords: `albumen-mcp` ([mcp/src/tools.rs](../mcp/src/tools.rs)) | unverified | Search by places and things [G1] | Custom and default metadata [F1] |
| Agent access over MCP | Yes: `albumen-mcp` ([AGENTS.md](../AGENTS.md)) | unverified | unverified | unverified |
| Exit codes 0–4 and a `next:` line on errors | on a branch, not released (branch `exit-codes`) | n/a | n/a | n/a |
| Face or people search | not yet | unverified | Face Groups, not in all regions [G2] | unverified |
| Review and approval with frame-accurate comments | not yet | unverified | unverified | Frame-accurate comments, annotations, share for review [F1] |
| Hosted sync service | not yet. Albumen copies to a disk or bucket you own. | Synced across devices [A1] | Automatic sync [G1] | Hosted storage [F1] |
| Gallery app | not yet ([ROADMAP.md](../ROADMAP.md)) | Photos app [A1] | Google Photos app [G1] | iOS and tvOS apps [F1] |

**unverified** means none of the pages above settles it either way. It is not a claim that the product lacks the feature.
