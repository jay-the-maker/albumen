# Scale

Scales with your hardware, no artificial limits.

There is no compiled ceiling on library size, file size, concurrency, or how many items a caller may ask for. A default is a soft default. The user can override it. Memory for a search or a browse is the page they asked for, not the library.

The files and sidecars stay the source of truth. A process-local metadata cache is a cache. Dropping it does not drop a caption or a GPS point.

## Soft defaults

| Default | Value | Override | What it is |
| --- | --- | --- | --- |
| Search page | 1000 | `limit`, or `ALBUMEN_SEARCH_PAGE` when `limit` is omitted | How many matches one `search` call returns. Pass `next_cursor` as `cursor` for the next page. No maximum. |
| Browse page | 200 | `limit`, or `ALBUMEN_BROWSE_PAGE` when `limit` is omitted | How many files one `browse` call returns. No maximum. |
| ExifTool batch | 64 | `ALBUMEN_EXIFTOOL_BATCH` | Files per ExifTool process. Not a limit on the library. |
| Metadata cache | 16384 records | `ALBUMEN_METADATA_CACHE_ITEMS` | Records kept in this process. `0` keeps every record read. A larger library is still searchable. |
| S3 workers | one per CPU | `ALBUMEN_S3_WORKERS` | Tokio workers for the S3 client. The SDK still applies network backpressure. No maximum. |

A page walks files in relative-path order and stops when the page is full. It does not load the library into a vector and then truncate it. Metadata for a page is read in batches, including the XMP or AAE sidecar beside a file, and records that are not on the page are not kept.

Under throttling the work slows and continues. A short page is a smaller answer, not a failure.

## Safety bounds that stay

These are not library ceilings. Each one protects the first principle or refuses unauthenticated input that could exhaust the process. Each one is configurable.

| Bound | Default | Override | Why it stays |
| --- | --- | --- | --- |
| Inbound MCP message | 8 MiB | `ALBUMEN_MCP_MAX_MESSAGE` | One JSON-RPC body from a client that has not been authenticated. The server checks the length before it allocates the body. Outbound pages are not cut to fit. |
| WebDAV header block | 64 KiB | `ALBUMEN_RECEIVE_MAX_HEADER` | Request headers on the camera listener, before the body is trusted. |
| FTP control line | 16 KiB | `ALBUMEN_RECEIVE_MAX_LINE` | One command line on the control connection. A missing newline must not grow without a bound. |
| Filename and path checks | reject empty, `.`, `..`, slashes, NUL, and control bytes | none | A stored name that contains a path separator would write outside the library. That is the first principle, not a size cap. |
| Symlink refusal | refuse to follow | none | A symlink can point at a file outside the library. Following it would copy or hash bytes the user did not put in the tree. |
| Scratch name in a temp path | 80 characters of the filename | `ALBUMEN_SCRATCH_NAME_CHARS` | Only the local temp path is shortened. The stored object keeps the original filename. |

Checksum verification, write-once mtime, and the refusal to overwrite a different file are first-principle checks. They are not size limits.

## What is not a ceiling

- The 1 MiB hashing buffer. The file is streamed. The buffer is not a maximum file size.
- A directory listing held while that directory is sorted. Page order has to be stable. The bound is the directory the user created, not a number in the source.
- The export manifest. `albumen export` writes one line per file because the manifest is the whole tree. Search and browse do not use that listing.
- Pair discovery reads the parent directory for a same-stem companion. It does not cap how many files may sit there.
- Takeout import. Zip and tgz members are streamed. There is no compiled cap on part count, member count, or member size. The 1 MiB copy buffer is the hashing buffer. Rejecting `..` and absolute member paths is the filename safety bound.

## Scale test

`mcp` tests build a synthetic library of 1,000,000 items. Each item is a computed path and, every 1,000th item, an empty-metadata caption. No files are created. The test pages a caption query to the end and checks that every match appears once, in order. It also searches for one path in the million. Peak RSS after a 32-item page of 100,000 items and after the same page of 1,000,000 items must stay within 32 MiB. The test runs in `cargo test` on every pull request.
