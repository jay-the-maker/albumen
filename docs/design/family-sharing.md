# Family sharing

**First principle: metadata and original quality are never lost.**

This is a design. No command in this tree shares a library yet. When it is built, it has to fit this page and [DESIGN_PHILOSOPHY.md](../../DESIGN_PHILOSOPHY.md).

A family library is the union of each member's originals. Each person keeps those files on storage they already have, or the family uses one bucket or one drive they already have. Albumen does not run a server for them, and the owner does not run one either. There is no account, no signup, and no background scan.

Every original is stored bit-exact. `copy_verify` streams the bytes, fsyncs, reads the destination back, and compares SHA-256. EXIF, XMP, IPTC, GPS, sidecars, Live Photo pairs, RAW+JPEG pairs, and motion photos stay. A shared file is that whole file. Albumen does not write a second, smaller copy in its place.

## Two ways to place the bytes

Both ways use the same namespaces, the same signatures, and the same rule that one member does not write another's files.

**Each member's own storage.** The member's disk, NAS, or object-storage prefix is the only place their originals are written. Other members receive a read capability for that namespace. The family view is the union of the namespaces a device can read. When one member's storage is unreachable, everyone else keeps working with the copies they have already verified.

**One shared bucket or drive.** The family already has the bucket or the drive. Each member has a prefix on it. Storage permissions let every member read, let each member create objects only in their own prefix, and grant no member a delete on another member's prefix. Albumen refuses those writes even when a credential is broader than that, so a wide token is not a way past the rule.

A synced folder the family already has is a filesystem. Albumen reads and copies it as files. It does not call an API that rewrites the file on the way through.

Leaving is still `albumen export --apply`, then `albumen verify-manifest`. The export is an ordinary tree of the original files.

## Namespaces

A library has an id, the SHA-256 of its genesis record. A member id is the hex encoding of that member's signing public key. A device id is a random id chosen on that device. Nothing here is an account name.

```text
albumen-family/<library-id>/
  genesis.json
  members/<member-id>/
    devices/<device-id>/
      log/<seq>.json
    objects/sha256/<hh>/<hex>
```

`<seq>` is a decimal counter for that device, with no gaps the writer intends. `<hh>` is the first two hex characters of the SHA-256, and `<hex>` is the full digest. The object path is the digest, so two different files never share a path, and the same bytes always name the same path.

The log is append-only. A new record is a new file, created with `create_new`. An existing log file or object is not opened for write, not replaced, and not unlinked. The camera filename is a field in the record and, on a local export, a sidecar beside the original. It is not a reason to overwrite an object that is already stored.

`genesis.json` is signed by the founder. It holds the library id, the founder's public key, and the placement (each member's own storage, or the shared bucket or drive, including a read locator that is not a secret). A later placement change is a new signed record. The genesis file stays.

## Originals

Adding a photo copies it into the author's namespace with the same check as `copy_verify`:

1. Stream the bytes into a new object. `create_new` refuses a path that already exists.
2. Hash the bytes while writing them.
3. Fsync.
4. Read the destination back and compare SHA-256.
5. On mismatch, remove that new object, name both hashes, and stop. The source stays. Nothing else in the namespace is removed.
6. Record the source mtime. On object storage that does not keep nanoseconds, the mtime lives in the signed record and in the sibling verify object the connectors already use. `Last-Modified` is the upload time and is not the photo's timestamp.

The signed record stores the SHA-256, the byte length, the camera filename, the mtime, and the pair or sidecar names. The bytes on disk stay the original. Embedded metadata stays inside the file. A caption or album membership written later is a log record, not a rewrite of the photo.

Pairs and sidecars are copied in the same pass. A Live Photo is still the still plus the video. A motion photo stays one file. An edit is a new file beside the original, in the same namespace, and the original remains.

A device that already has an object with the same SHA-256 does not write it again. The record still says which member holds it.

## Albums and metadata

Albums, shared captions, and hides are log records. Each device signs its own records and links them in a hash chain: every record names the SHA-256 of the previous record on that device. Readers check the signature and the link. A record that fails either check is not applied.

The family view is the merge of every admitted device log the reader can verify. There is no central sequence. Merge is a CRDT:

| Record | Merge |
| --- | --- |
| Object added | Union, keyed by SHA-256. Two records with the same digest must carry the same byte length. A mismatch is an error and neither object is treated as verified. |
| Album membership | Observed-remove set. An add has a unique id (member, device, sequence). A remove names the add ids that member had seen. An add they had not seen stays. Membership points at a SHA-256. It does not own the bytes. |
| Album title | Multi-value register. Concurrent titles all stay. A later record may mark one title as chosen; the earlier titles remain in the log. |
| Caption or keyword | Kept per author and per SHA-256. Two members' captions on the same photo are both shown. Neither replaces the embedded metadata inside the file. |
| Hide | Per viewer. See below. |

Search reads the local originals and these records. It does not build a second catalog that is the only copy of a caption or a GPS point. The file and the log can rebuild a view.

## Nobody writes another member's photos

Two checks, both required:

1. **Namespace ownership.** A record is accepted only when the signature matches the member id in its path. Album operations and captions are signed by their author. They reference another member's object by SHA-256. They do not open that object for write.
2. **Storage permissions.** On a shared bucket or drive, the member's credential can create objects only under `members/<their-id>/`. It cannot create, replace, or delete under another member's prefix. On each-member storage, other people receive a read capability only. Albumen refuses to write outside the caller's namespace even if the credential would allow it.

Removal is a hide or a tombstone, and it is per viewer.

A hide is a record in the viewer's own log: the viewer's member id, the SHA-256, and `hidden: true`. A later record from that same viewer with `hidden: false` shows the file in that viewer's library again. Both records stay in the log. Other members do not read that hide as their own. Their bytes, mtime, filename, and log are unchanged.

An unshare is a record in the author's own log that says the author no longer offers that SHA-256 in the family view. Clients that verify the log stop offering it to members who do not already have a verified local copy. The object stays in the author's namespace. A member who already `copy_verify`'d it keeps that copy. Unshare does not reach into their disk.

There is no delete tool, no move, and no quarantine of a member object. `BlobStore::delete` remains what it is today: rollback of a destination this process just wrote when verification fails.

## Key management

Each member generates a signing key on their own machine. Ed25519 is the signature scheme. The public key is the member id. The private key stays in a file the member holds, mode `0600`, and they may copy that file to their own devices. Albumen does not upload the private key, and it does not have an account to recover it. A member who loses the key keeps every object already stored. They create a new key, and an admitted member signs an admit that names the new key as a successor. The old key stops being accepted for new records once that admit is verified. Old signatures remain valid for the records they already signed.

The sane default stores original bytes in the clear when the storage permissions already limit readers to the family. The signed SHA-256 is then the digest of the object itself.

Sealing is off unless the storage read set is wider than the current members. When it is on, the object in the namespace is the original bytes sealed with a content key. The signed record carries the plaintext SHA-256 and the sealed object's SHA-256. A reader checks the sealed object, opens it, and checks the plaintext. The plaintext is the original, with metadata intact. The member's local library stores that plaintext, bit-exact. The content key is wrapped to each admitted member's public key and stored as a log record. A revoke rotates the wrapping for objects added after the revoke, so a revoked key does not unwrap those later objects. Objects already copied stay with the member who copied them.

## Invites and revocation

There is no account. An invite is a file two people exchange, or a record one member's device can read.

1. The invitee generates a keypair. The public key is their member id.
2. An admitted member signs an invite that names the library id, the invitee's public key, a one-time token hash, and an expiry. The inviter's log stores that record. The invite file the invitee receives contains the token and the inviter's public key.
3. The invitee appends a hello in their own namespace, signed, naming the token hash. They do not write into the inviter's namespace.
4. The inviter appends an admit naming that public key and that token hash. Readers accept the new member only when the admit verifies and the token hash has not been admitted before. A second admit of the same token is ignored.
5. On a shared bucket or drive, the storage owner creates the invitee's prefix and a credential that can create objects only there, and takes write access away from everyone else on that prefix. Albumen prints that change. It does not rewrite objects to apply it.

Revocation is another signed record. The founder, the inviter, or the member themselves may sign it. After a reader has verified the revoke, new records from that key are not applied. Records and objects already verified stay. The storage owner removes the write credential. The revoked member's namespace remains readable so the library does not lose those originals. A revoked member keeps every local copy they already verified, and they can `albumen export` it.

A self-leave is the member signing their own revoke. It is the same record. It does not delete their photos.

## Conflicts

One member's devices never need a lock server. Each device has its own hash chain. The member's view is the union of those chains. Two devices that were offline both append, and both chains verify, so both sets of records stay.

Across members, the photo's identity is the SHA-256.

- The same bytes from two members are one original and two custody records. Neither copy is discarded. A reader stores one local file and keeps both records.
- Different bytes under the same camera filename are two originals. Both stay. The camera filename is stored in each record, byte-exact. The object path is the digest, so the files do not overwrite each other.
- An album add and a concurrent album remove merge by the observed-remove set. The remove drops only the adds it names. A concurrent add remains. The file's bytes are not part of that merge.
- Two captions stay. Two album titles stay until a member appends a choice. The log still contains the other title.
- A hash mismatch on a path that claims a digest is a failed `copy_verify`. The new object is removed. The existing object and the source stay.

Wall-clock timestamps are claims inside records. They are not the order of the log. Order on one device is the hash chain. Order across devices is partial, which is what the CRDT merge uses.

## Offline

Each device keeps a local library of the originals it has `copy_verify`'d, plus the log records it has verified. Search and browse read those files. They do not phone home.

While storage is unreachable, the device still appends to its own local log and still copies new camera files into the local library. The camera roll is unchanged. Nothing is deleted on the device to make room.

When storage is reachable again, the device writes its new log records and new objects with `copy_verify`, then reads the other namespaces, checks signatures and hash chains, and copies any missing object the same way. A missing member storage means that member's newest originals are not in this sync. Copies already verified remain usable. The sync does not fail the whole library because one namespace is absent.

No process has to stay running for the family. Sync is a command the member runs, or a later client that runs when they ask it to. It is not a scan they did not start.

## Threat model

| Actor | What Albumen relies on | What still holds |
| --- | --- | --- |
| Another family member | Their client and their storage credential | They can read what has been shared. They cannot create, replace, or delete objects in someone else's namespace. A hide they sign changes only their view. |
| A revoked member who still has an old credential | The storage owner removes the write credential. Clients that have seen the revoke ignore new records from that key. | Objects already on their disk stay theirs. Objects added after a key rotation are not unwrapped by the revoked key. Clients that have not yet seen the revoke can still accept a record until they do. |
| A reader who is not an admitted member | Storage permissions and the admit log | A namespace they can read is still not a membership. Readers apply records only from keys that genesis or an admit has accepted. |
| Someone who can write a prefix but has no admit | Signature checks | Their objects and log files may sit on disk. The family view ignores them. They do not become members. |
| A stolen invite file | One-time token hash on the admit | After the first admit, a replay is ignored. An invite past its expiry is ignored. |
| A compromised member key | Revoke, and a successor admit | Older records keep their signatures. New records from the compromised key are ignored once the revoke is verified. |
| The storage host | Permissions the family set, and local copies | Albumen does not claim the host cannot delete objects on its own. Each member who has `copy_verify`'d a file still has those bytes. `verify-manifest` checks a local export. Sealing protects a host that can read but is outside the family, when the family turns sealing on. |

A checksum mismatch names both hashes. A bad signature names the member id and the record path. Those failures are the report. They are not a reason to skip the check.

## Test plan

Synthetic fixtures only. No real photos, no real names, no account service.

- Genesis verifies, and a genesis file with a broken signature is refused.
- Member A `copy_verify`s a synthetic JPEG and its sidecar into A's namespace. The destination SHA-256, byte length, and mtime match. The source bytes and mtime are unchanged. Embedded metadata is unchanged.
- Member B reads A's object and verifies the digest. B's write into A's prefix fails at the storage permission. With a deliberately broad credential, Albumen still refuses the write.
- B's hide writes one tombstone under B's log. A's object bytes, mtime, and log are unchanged. A's view still shows the file. B's view hides it. B's later unhide shows it again, and both records remain.
- A's unshare stops a client that has verified the record from offering the file to a member who does not already have it. A's object remains. A member who already verified a local copy still has that copy.
- Concurrent album add and remove: the add the remover had not seen stays in the album. The object bytes stay in the author's namespace.
- Two synthetic files with the same camera filename and different bytes both remain, with both digests. The same bytes from two members are one object and two custody records.
- Invite and admit use two key files and no network to an identity service. Replaying the token does not create a second member.
- After a revoke, a new record from that key is ignored, and an object already verified still matches its SHA-256.
- Two devices of one member, each with a log written while storage is unreachable, merge by union when storage is back. Every verified object is present. `copy_verify` covers the sync.
- `albumen export --apply` of the local library and `albumen verify-manifest` succeed. The export is ordinary files.
- The implementation has no tool named delete, remove, unlink, move, or quarantine for a member object. A failed `copy_verify` removes only the new object that check just wrote.

The pull-request suite uses a small synthetic library. A large shared library is a later measured run, on a schedule or by hand, the way the other large runs are. It does not sit on the pull-request path.

## What this design will not do

- Run a server, or ask the owner to run one.
- Create an account, send mail, or require a signup.
- Recompress, transcode, or strip an original so it can be shared.
- Let one member replace, unlink, or rewrite another member's objects.
- Treat a hide, a tombstone, or an unshare as a delete of bytes.
- Overwrite an object because a filename collided.
- Build a catalog that is the only copy of a caption, a GPS point, or an album.
- Nag, upsell, or scan storage the member did not ask to sync.
