Albumen is the open-source alternative to iCloud Photos, Google Photos and Frame.io.

# How Albumen compares

Every Albumen row points at a file or command on `main`.

- **on a branch, not released**: the work is pushed to a branch and is not merged into `main` yet.
- **not yet**: Albumen does not have the feature.

Every competitor price and feature claim comes from a vendor page fetched on 2026-10-29 and linked below. Each vendor cell names its source. A claim no fetched page backs is marked **unverified**. Vendor features use the vendor's own names. Prices are US prices before tax; check the linked page for your country.

Sources, all fetched 2026-10-29:

- [A1] Apple, Set up and use iCloud Photos: https://support.apple.com/en-us/108782
- [A2] Apple, iCloud+ plans and pricing: https://support.apple.com/en-us/108047
- [A3] Apple, How to use iCloud Shared Photo Library on iPhone or iPad: https://support.apple.com/en-us/118229
- [A4] Apple, Find People and Pets in Photos on your iPhone or iPad: https://support.apple.com/en-us/108795
- [A5] Apple, Search for photos and videos on iPhone: https://support.apple.com/guide/iphone/search-for-photos-and-videos-iph392d77d5f/ios
- [A6] Apple, Transfer a copy of your iCloud Photos collection to another service: https://support.apple.com/en-us/118257
- [A7] Apple, Transfer a copy of your photos and videos to iCloud Photos: https://support.apple.com/en-us/120924
- [G1] Google Photos Help, Back up photos & videos: https://support.google.com/photos/answer/6193313
- [G2] Google Photos Help, Set up & manage your face groups: https://support.google.com/photos/answer/6128838
- [G3] Google One plans: https://one.google.com/about/plans
- [G4] Google Photos Help, Set up partner sharing: https://support.google.com/photos/answer/7378858
- [G5] Google Photos Help, Transfer iCloud photos and videos to Google Photos: https://support.google.com/photos/answer/10502587
- [G6] Google Account Help, How to download your Google data: https://support.google.com/accounts/answer/3024190
- [G7] Google Photos Help, Copy your photos & videos to a service outside of Google: https://support.google.com/photos/answer/9666875
- [F1] Frame.io pricing and FAQ: https://frame.io/pricing
- [F2] Frame.io, Storage Connect for Frame.io: https://help.frame.io/en/articles/9179936-storage-connect-for-frame-io
- [F3] Frame.io, Getting started in the iOS app: https://help.frame.io/en/articles/9105533-getting-started-in-the-ios-app
- [F4] Frame.io, Enhanced search with AI Search: https://help.frame.io/en/articles/9101079-enhanced-search-with-ai-search

## Price

| | Albumen | iCloud Photos | Google Photos | Frame.io |
|---|---|---|---|---|
| Cost | Free, Apache-2.0 ([LICENSE](../LICENSE)). You supply the disk or bucket. | 5 GB free. iCloud+ 50 GB $0.99/mo, 200 GB $2.99/mo, 2 TB $9.99/mo, 6 TB $29.99/mo, 12 TB $59.99/mo [A2] | 15 GB free per Google account. Basic 100 GB $1.99/mo, Google AI Plus 2 TB $9.99/mo [G3] | Free plan with 2 GB. Pro $15 per member per month, Team $25 per member per month, Enterprise custom [F1] |
| Account needed | No. The CLI runs locally (`albumen --help`). | Apple Account [A1] | Google Account [G1] | Members need an account; outside reviewers use share links without one [F1] |

## Features

| Feature | Albumen | iCloud Photos | Google Photos | Frame.io |
|---|---|---|---|---|
| Integrity proof you can re-check (SHA-256 manifest, re-verify, proof file) | `albumen export` writes a SHA-256 manifest and reads every copy back ([cli/src/main.rs](../cli/src/main.rs), [ingest/src/local.rs](../ingest/src/local.rs)). `albumen verify-manifest` re-checks it later. The proof file (`verify-manifest --proof`) is on a branch, not released (branch `verify-manifest-proof`). | none documented¹ | none documented¹ | none documented¹ |
| Import from Google Takeout | Yes: `albumen import-takeout` ([docs/importers/takeout.md](importers/takeout.md)). Albumen keeps what Takeout gives, bit-exact. It cannot add back anything Takeout leaves out. | Transfer from Google Photos to iCloud Photos, started in Google Takeout. Motion Photos and Live Photos are not transferred [A7] | Google Takeout export of Photos [G6]. Transfer to another service leaves out the video portion of Motion Photos and Live Photos [G7] | unverified |
| Compare Takeout JSON and EXIF time and GPS | on a branch, not released (`albumen takeout-fidelity`, branch `takeout-fidelity`) | unverified | Takeout puts metadata that is not in the original file, such as comments, in a separate JSON file [G6] | unverified |
| Import from iCloud or a Photos library | Yes: `albumen import-icloud` ([docs/importers/icloud.md](importers/icloud.md)) | Download copies of your library [A1]. Transfer to Google Photos sends only the most recent edit, not the original, and does not transfer Live Photos [A6] | Transfer from iCloud. The video portion of Live Photos does not transfer [G5] | unverified |
| Live Photos kept | On a branch, not released (branch `live-photo-cross-ecosystem`): a test takes a synthetic HEIC + MOV Live Photo in through `albumen import-icloud`, out through `albumen export`, and back in through `albumen import-takeout`. After every step both files are SHA-256 identical, the HEIC is not converted, the two files are found together as one asset, no extra files are written, the source folders are unchanged, and the ExifTool metadata is unchanged, including synthetic EXIF GPS and OffsetTimeOriginal ([ingest/tests/cross_ecosystem_live_photo.rs](../ingest/tests/cross_ecosystem_live_photo.rs)). Pairing by ContentIdentifier is checked on the iCloud import only; later steps pair by file name. Albumen does not play Live Photos. | Stored in original format [A1] | Backed up from the iPhone or iPad app [G1] | unverified |
| Copy to your own S3 bucket | Yes: `albumen export --to s3://bucket/prefix` ([ingest/src/s3.rs](../ingest/src/s3.rs)) | unverified | Takeout delivers by email link, Drive, Dropbox, OneDrive or Box; S3 is not on the list [G6] | Storage Connect: your own Amazon S3 bucket in us-east-1, Enterprise only [F1][F2] |
| Family library | `albumen family` ([cli/src/family.rs](../cli/src/family.rs)) builds a shared family library from signed logs, with no account, and writes nothing without `--apply` or `--execute`. | Shared Library, with up to five other people [A3] | Partner sharing, with one partner at a time [G4] | Workspaces and project members [F1][F3] |
| Camera upload | `albumen-receive` over WebDAV, SFTP and FTP ([README.md](../README.md#camera-receiver)) | unverified | unverified | Camera to Cloud [F1] |
| Phone camera-roll backup | Android app and iOS library ([docs/mobile-android-backup.md](mobile-android-backup.md), [mobile/ios](../mobile/ios)) | Automatic upload [A1] | Automatic backup [G1] | Upload from the photo library in the iOS app, picked by hand [F3] |
| Metadata search | By date, camera, lens, GPS box and keywords: `albumen-mcp` ([mcp/src/tools.rs](../mcp/src/tools.rs)) | Search by date, place, category, text and caption [A5] | Search by places and things [G1] | Search by status, keywords, uploader, file type, format and more [F4] |
| Face or people search | not yet | People & Pets [A4] | Face Groups, not in all regions [G2] | AI Search has no face detection [F4] |
| Review and approval with frame-accurate comments | not yet | unverified | unverified | Frame-accurate comments, annotations, share for review [F1] |
| Hosted sync service | not yet. Albumen copies to a disk or bucket you own. | Synced across devices [A1] | Automatic sync [G1] | Hosted storage [F1] |
| Gallery app | not yet ([ROADMAP.md](../ROADMAP.md)) | Photos app [A1] | Google Photos app [G1] | iOS and tvOS apps [F1] |

¹ None of the three vendors documents an equivalent on the pages above. The same holds for three more Albumen features, which the pages above leave unverified for all three vendors: a read-only scan of an existing library (`albumen scan`, [cli/tests/scan_source_untouched.rs](../cli/tests/scan_source_untouched.rs)), a local account of every file and connection (`albumen ledger`, [ingest/src/ledger.rs](../ingest/src/ledger.rs)), and agent access over MCP (`albumen-mcp`, [AGENTS.md](../AGENTS.md)).

**unverified** means none of the pages above settles it either way. It is not a claim that the product lacks the feature.

`.github/workflows/link-check.yml` checks every link on this page.
