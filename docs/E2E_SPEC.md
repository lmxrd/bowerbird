# End-to-end-encrypted folders: the format

The specification of Organizer's encrypted folders (the design and the
decisions: [M6E_DESIGN.md](M6E_DESIGN.md)). Every client implements it with
its platform's own primitives — WebCrypto (web), CryptoKit and CommonCrypto
(iOS), the JDK (Android), RustCrypto (desktop, the reference) — and checks
itself against the same test vectors (§9). The reference implementation is
`packages/e2e-crypto` (`src/keys.rs`, `folder.rs`, `names.rs`,
`contents.rs`); where this text and it disagree, it is a bug in one of them.

## 1. Overview

- Each user has an **identity key pair** (X25519). The public key is on the
  server in plain; the private key only **wrapped**, twice: under a key
  derived from their **passphrase** (§2) and under their **recovery key**
  (§3). The login (password, passkey, single sign-on) is never a key.
- Each encrypted folder has **folder keys**, 32 random bytes each, numbered
  by **generation** (1, 2, ...). A folder key is wrapped for each **member**
  (the owner and everyone the folder is shared with) with their public key
  (§4). A new generation is made when a member loses access; what is
  written after uses it, what was written before keeps its own.
- Under an encrypted folder, every **name** (§6) and every file's
  **contents** (§5) are encrypted with keys derived from the folder key. The
  encrypted folder's own name is not: it is what a device without the key
  shows (a lock, "Unlock to see what's inside").
- The server keeps all of it and can open none of it. It checks what it can
  (sizes, the storage limit, permissions, members, generations) and refuses
  what would need the plaintext (§7).

**What it protects against:** someone who reads the server's disk, its
backups or its database — an admin, a thief, a leaked backup — learns the
encrypted folders' sizes, their tree's shape, the times and who the members
are; not the names or contents. **What it does not:** a server that is
changed to lie (to hand out a public key of its own when a member is added,
or to serve a changed web app) can get at what is shared after that; the
apps do not compare keys out of band yet. A member keeps whatever they
copied while they had access.

**Primitives:** X25519 (RFC 7748), HKDF-SHA256 (RFC 5869), AES-256-GCM
(12-byte nonces, 16-byte tags), PBKDF2-HMAC-SHA256. **Encodings:** binary
values are unpadded base64url (RFC 4648 §5) in the API; strings are UTF-8;
integers in binary formats are big-endian. All `info` and associated-data
strings below are ASCII, without a terminator.

## 2. The identity keys and the passphrase

- **Private key:** 32 random bytes (clamped when used, as X25519 does).
  **Public key:** X25519(private, 9), 32 bytes.
- **Passphrase key:** PBKDF2-HMAC-SHA256 of the passphrase's UTF-8 bytes
  **in Unicode NFC**, with a random 16-byte **salt**, `iterations` rounds,
  32 bytes out. New passphrases use **600,000** iterations; a reader takes
  the count stored with the key, and refuses fewer than **100,000**.
- **Wrapped private key** (61 bytes):

  | Bytes | |
  | --- | --- |
  | 1 | version, 1 |
  | 12 | nonce, random |
  | 32 + 16 | AES-256-GCM(key, nonce, private key, ad = `organizer-e2e/1/private-key`) |

  With the passphrase key, it is `passphrase.wrapped`, kept with
  `passphrase.salt` and `passphrase.iterations`. A wrong passphrase fails
  the tag. Changing the passphrase wraps the same private key under a new
  salt and passphrase.

## 3. The recovery key

- 32 random bytes, made at set-up, shown once.
- **Shown** as RFC 4648 base32 (uppercase, no padding): 52 characters (the
  last carries one bit and four zero bits, so it is `A` or `Q`), in 13
  groups of 4 joined by `-`, e.g. `ABCD-EFGH-...-WXYQ`.
- **Read** with dashes and white space ignored and lowercase taken as
  uppercase; anything else (another length, another character, unused bits
  that are not zero) is refused.
- **Recovery wrapping key:** HKDF-SHA256(ikm = the 32 bytes, salt = empty,
  info = `organizer-e2e/1/recovery`), 32 bytes. The private key is wrapped
  under it as in §2: `recovery_wrapped`.
- With it, a user who forgot the passphrase unlocks the private key and
  sets a new passphrase. **If the passphrase and the recovery key are both
  lost, nobody can read the encrypted folders: not the user, not an admin,
  not us.**

## 4. Folder keys

- A **folder key** is 32 random bytes, of a **generation** (1, 2, ...).
- **Wrapped for a member** whose public key is `P` (93 bytes):

  1. `e` = 32 random bytes (an ephemeral private key), `E` = X25519(e, 9).
  2. `shared` = X25519(e, P); all zeros is refused.
  3. `key` = HKDF-SHA256(ikm = shared, salt = E ‖ P, info =
     `organizer-e2e/1/folder-key`), 32 bytes.
  4. `ad` = the folder's id (its 16 UUID bytes) ‖ generation (4 bytes).

  | Bytes | |
  | --- | --- |
  | 1 | version, 1 |
  | 32 | E |
  | 12 | nonce, random |
  | 32 + 16 | AES-256-GCM(key, nonce, folder key, ad) |

  Unwrapping: `shared` = X25519(private, E), `P` = the holder's own public
  key, the rest the same. The folder's id and the generation are bound: a
  wrapping moved to another folder or generation fails.
- The **folder** is the encrypted folder's top (the node whose `e2e_root`
  is its own id); everything under it uses its keys. Encrypted folders do
  not nest.
- **Generations.** A new encrypted folder has none (`generation` 0,
  `needs_new_key`): its owner's device makes generation 1 and posts it.
  When a member loses access (a share removed, a group left), the server
  drops their wrapped keys and sets `needs_new_key`; the next member's
  device makes the next generation, wrapped for every member left who has a
  public key, in one request. New names and contents use the newest
  generation; older ones stay readable with their own keys, which members
  keep.
- **Members waiting.** A member who has not set up encryption has no public
  key: they are listed, and waiting ("Waiting for them to set up
  encryption"). Once they have one, any member's device that holds the keys
  wraps every generation for them (`GET /encryption/pending` says where).

## 5. Contents

An encrypted file is a header and segments:

| Bytes | |
| --- | --- |
| 1 | version, 1 |
| 4 | generation of the folder key |
| 32 | file nonce, random per write |
| ... | segments |

- **File key:** HKDF-SHA256(ikm = the folder key, salt = the file nonce,
  info = `organizer-e2e/1/file`), 32 bytes. A new version of a file gets a
  new file nonce, so a new key.
- **Segments:** the plaintext in pieces of **65,536 bytes**, the last one 0
  to 65,536 bytes (an empty file is one empty last segment; a file of
  exactly 65,536 bytes is one full last segment). Segment `i` (from 0) is
  AES-256-GCM(file key, nonce_i, piece, ad = empty), its 16-byte tag after
  it, where **nonce_i** = 7 zero bytes ‖ `i` (4 bytes) ‖ 1 for the last
  segment, else 0.
- A reader knows a segment is the last when nothing follows it; a segment
  that is cut, reordered, dropped, added after the last, or flagged wrongly
  fails its tag.
- **Sizes:** encrypted = 37 + plaintext + 16 × max(1, ⌈plaintext / 65,536⌉).
  The server sees (and counts against the storage limit) the encrypted size;
  clients show the plaintext size, worked out the other way.
- Streams: a writer encrypts as it reads (uploads in segments, resumable
  for large files); a reader decrypts a segment at a time and should write
  to a temporary file, renamed when the last segment checked.

## 6. Names

- **Name key:** HKDF-SHA256(ikm = the folder key, salt = empty, info =
  `organizer-e2e/1/name`), 32 bytes.
- The name in Unicode **NFC**, as UTF-8, at most **158 bytes** (so the
  encrypted name fits the 255 bytes of most filesystems).
- **Encrypted name:** unpadded base64url of

  | Bytes | |
  | --- | --- |
  | 1 | version, 1 |
  | 4 | generation of the folder key |
  | 12 | nonce, random |
  | n + 16 | AES-256-GCM(name key, nonce, name, ad = the parent folder's id, 16 bytes) |

- The **parent's id** is bound: a name moved by the server into another
  folder fails. So a **move** inside an encrypted folder sends the new
  parent and the name encrypted again for it, in one request; a rename
  sends a new encrypted name. Nothing moves in or out of an encrypted
  folder (the server refuses): a copy, decrypted or encrypted by the device,
  is the way.
- The same name encrypts differently each time: the server cannot see that
  two names are the same, so **devices check for clashes** (decrypting the
  folder's names) before they upload or rename.
- The encrypted folder's own name is plain (§1).

## 7. The server's part

The API (`apps/server/src/routes/encryption.rs`, in the OpenAPI spec
under the tag Encryption):

| | |
| --- | --- |
| `GET /api/encryption` | The user's `public_key`, `passphrase` (`salt`, `iterations`, `wrapped`) and `recovery_wrapped`; `set_up` false until set up. A session or an API key. |
| `POST /api/encryption` | Set up, once (a session). |
| `PUT /api/encryption/passphrase` | A new passphrase wrapping (a session). |
| `POST /api/files/{id}/folders` with `encrypted: true` | A new encrypted folder, in the user's own files, not in another one, once set up. |
| `GET /api/encryption/folders/{id}` | For the encrypted folder `id` is in: `generation`, `needs_new_key`, the user's wrapped `keys` of each generation, the `members` with their public keys and the generations they lack. In a mirror (docs/M7D_DESIGN.md §9), the keys the mirror kept of the user's own (for a node of the copy, or for the drive's folder by `source_id` once the user cannot read it there): `folder_id` is the drive's folder, the id to unwrap with; no `members`. |
| `POST /api/encryption/folders/{id}/keys` | Wrapped keys for members: of existing generations for those who lack them (kept if they have one), or the next generation for every member with a public key. |
| `GET /api/encryption/pending` | The encrypted folders that need a new key or have members waiting, for a device to see to. |

Every node has `e2e_root`: the encrypted folder it is in (its own id for
that folder), null for the rest; the change feed says it too, and a trash
item has it with `original_parent_id` (its name's associated data).

What the server refuses or leaves out for encrypted folders, because it
would need the plaintext: text extraction and the search index (not even
the names), the AI (the drop box never proposes, reads or places into
them), server previews (there are none; their files have no type),
**public links** (refused on them, and left out of a plain folder's link
and its zip), **WebDAV** (hidden: left out of listings, not found by path),
moving anything across their edge, an encrypted folder inside another,
sharing something inside one (the folder is shared whole), placing a share
inside one. What works on the opaque bytes as on any file: versions, the
trash (with their ids kept, so their names still open after a restore; a
file inside goes back only into its own folder), the storage limit, sharing
with people and groups, the change feed and sync, backups (still
encrypted).

## 8. Versions of the format

The first byte of every wrapped key, encrypted name and encrypted file is
the format's version, **1** for everything here; the `info` and associated
data strings carry it too (`organizer-e2e/1/...`). A reader refuses a
version it does not know rather than guess. A new version is a new number
everywhere it changes, with vectors of its own; readers keep reading the
old one, and writers move to the new one once every client reads it.

## 9. Test vectors

`packages/e2e-crypto/vectors.json`, written from fixed inputs by
`mise run gen:vectors` (`cargo run -p organizer-e2e-crypto --example
vectors`); the crate's test `vectors_are_up_to_date` fails when the file and
the implementation differ. Bytes are hex. Each client's tests read it:

- `identities`: `private` → `public` (X25519).
- `passphrases`: `passphrase` (one decomposed: NFC first), `salt`,
  `iterations` → the PBKDF2 `key`; `private` wrapped with `nonce` →
  `wrapped`, which unwraps to `private`.
- `recovery_keys`: `key` ↔ `text`; `wrapping_key`; `private` wrapped with
  `nonce` → `wrapped`.
- `folder_keys`: `folder_key` of `generation` for `folder_id`, wrapped for
  `recipient_public` with `ephemeral_private` and `nonce` → `wrapped`; it
  unwraps with `recipient_private`, and not with another generation.
- `names`: `name` (one decomposed) in `parent_id` with `nonce` →
  `encrypted`, which decrypts to `decrypted` (the NFC form) and not with
  another parent; `name_key`.
- `contents`: plaintexts of `plain_length` bytes, byte `i` = `i % 251`,
  sealed with `folder_key`, `generation` and `file_nonce` → `file_key`,
  `encrypted_length`, `encrypted_sha256` (and `encrypted`, whole, for the
  small ones); 0, 13, 65,536 and 132,072 bytes, so the empty, short, exactly
  one segment and several segments cases are all there.
- `backend_secrets` (§11): `secret` (one not ASCII) of `owner_id`, sealed
  for `recipient_public` with `ephemeral_private` and `nonce` → `sealed`
  (and `sealed_base64url`); it opens with `recipient_private`, and not for
  another owner id.

## 10. Decrypting by hand: `organizer-decrypt`

A small command line tool, from the reference implementation, so that
nobody depends on the apps to read their own files. It decrypts one
encrypted folder (the one with the lock) and everything in it into a folder
of your choice, with your passphrase or your recovery key. It writes
nothing to the server and overwrites nothing.

**Get it:** `mise run build:decrypt` builds `dist/organizer-decrypt` (or
`cargo build --release -p organizer-e2e-crypto --bin organizer-decrypt`).

**From a running server:**

1. In Organizer, Settings → API keys: make a key (read is enough) and copy
   it.
2. Run, with the folder's path as you see it in your files:

   ```sh
   organizer-decrypt --server https://files.example.com --api-key org_... \
     --folder /Private --out ./Private
   ```

3. Type your encryption passphrase when asked (or set
   `ORGANIZER_PASSPHRASE`). With `--recovery-key`, type the recovery key
   instead (or set `ORGANIZER_RECOVERY_KEY`); dashes and case do not
   matter.

A folder shared with you works the same way, by the path where you added it
to your files, or by its id with `--folder-id`.

**From a restored backup** (the server need not run):

1. Restore the backup's database copy and the files (with `organizer
   backup restore`, or restic) somewhere. The database is the file the
   backup made with `VACUUM INTO` (`backup/staging/...`); put it at
   `<data>/organizer.db`, or give it with `--database`.
2. Run, with the email of the user whose keys open it:

   ```sh
   organizer-decrypt --data-dir /restore/data --user me@example.com \
     --folder /Private --out ./Private
   ```

   Your files folder is found as the server would find it (the folder
   chosen in the settings, else `<data>/users/<your id>/files`); give it
   with `--storage-url` when it was restored elsewhere, or when it is not
   the default under `STORAGE_URL`.
3. Type the passphrase (or `--recovery-key`) as above.

It prints how many files and folders it decrypted; anything it could not
(a name or contents that fail, a file already in `--out`) is listed and
left out, and the exit status is 1. A wrong passphrase or recovery key
decrypts nothing.

## 11. Private credentials: a storage backend's sealed secret

A user's own storage backend with **private credentials**
(docs/M7C_DESIGN.md §3) keeps its secret (the S3 secret access key)
sealed to its owner's identity key, so that the server cannot read it at
rest. It is the folder keys' wrapping (§4) with another `info`, another
associated data and a plaintext of any length:

1. `e` = 32 random bytes, `E` = X25519(e, 9); `P` = the owner's public key.
2. `shared` = X25519(e, P); all zeros is refused.
3. `key` = HKDF-SHA256(ikm = shared, salt = E ‖ P, info =
   `organizer-e2e/1/backend-secret`), 32 bytes.
4. `ad` = the owner's user id (its 16 UUID bytes).

| Bytes | |
| --- | --- |
| 1 | version, 1 |
| 32 | E |
| 12 | nonce, random |
| n + 16 | AES-256-GCM(key, nonce, the secret's UTF-8 bytes, ad) |

The API takes it as `sealed_secret`, unpadded base64url, at most 4,096
characters (so a secret of at most about 3,000 bytes). Opening:
`shared` = X25519(private, E), the rest the same; a sealing moved to
another user fails. The client that adds the backend seals the secret
with the user's own public key and sends the plain secret too, once, for
the connection test and to unlock it for that session; afterwards, each
time the user unlocks encryption, the client opens the sealed secret and
hands it to the server for that session (`PUT /backends/{id}/unlock`),
which keeps it in memory only. The reference is
`packages/e2e-crypto/src/secrets.rs`.

## 12. Encrypted folders shared with another server (`organizer-e2e`)

An encrypted folder may be shared with someone on another Organizer
server whose OCM discovery advertises the capability `organizer-e2e`
(docs/M7E_DESIGN.md §6, §10); never with any other server. **Nothing of
the format changes**: no new key, derivation or associated data (so no new
vectors). The member's folder keys are wrapped as in §4 with the owner's
folder id, and the names under it are sealed as in §6 with the owner's
parent ids. What is added is who keeps what, and how the ids travel.

**On the owner's server** the member is the share with them (not a user):

- `GET /api/encryption/folders/{id}` lists them among the `members` with
  `remote` = their address (`user@host`) and `user_id` = the share's id
  (the `Grant.id` the Share dialog shows); `public_key` is the one their
  server gave (null until they set up encryption there), `missing` as for
  anyone. `GET /api/encryption/pending` lists the share's id in `waiting`.
- A member's device wraps the keys for them exactly as for a local
  member, with that public key, and posts them to
  `POST /api/encryption/folders/{id}/keys` with `user_id` = the share's id.
  A new generation must include them when their public key is known.
- The wrapped keys stay on the owner's server. When the share ends
  (removed, declined, its folder trashed, the giver's access gone, their
  user removed there), their keys go and the folder `needs_new_key`: the
  next generation is not wrapped for them.

**Between the two servers** (POSTs under the owner's and the member's OCM
`endPoint`, signed with HTTP Message Signatures as every OCM request,
answered only to a server that advertises `organizer-e2e`):

| | |
| --- | --- |
| `POST /ocm/organizer-e2e/public-key` (to the member's server) | `{"senderDomain", "providerId", "shareWith"}`: the share's id at the owner's server and the member's address → `{"publicKey": "<base64url>" \| null}`. Asked when the share is made and, while null, again when the owner's devices look (at most once a minute); kept once given. |
| `POST /ocm/organizer-e2e/keys` (to the owner's server) | `{"senderDomain", "providerId"}` → `{"folderId", "generation", "keys": [{"generation", "wrapped"}]}`: the encrypted folder's id on the owner's server, its newest generation, and the member's wrapped keys. 404 once the share is gone or no longer reaches the folder. |

**The owner's ids.** The owner's WebDAV answers a `sync-collection`
REPORT on a share of an encrypted folder with each member's id,
`<o:id>` in its `prop`, and the top's, `<o:root-id>` in the multistatus
(`xmlns:o="urn:x-organizer:dav"`). The member's server reads them after
each scan of the share (and at once after a folder is made through it)
and keeps them as `Node.source_id` of its own index.

**On the member's server** the folder is a drive of kind `remote` whose
top is an encrypted folder (`e2e_root` its own id). Its clients open it as
they open a mirror's copy (docs/M7D_DESIGN.md §9):

- `GET /api/encryption/folders/{id}`, for a node of it or by its
  `source_id`, asks the owner's server for the user's keys and answers
  them with `folder_id` = the owner's folder (unwrap with it),
  `members` empty and `needs_new_key` false; the member's devices post no
  keys and get no tasks for it. 502 `remote_unreachable` when the owner's
  server does not answer.
- Names are opened, and sealed for a new name, rename or move, with the
  parent's `source_id`, not its id here. A node whose `source_id` is still
  null has not been seen on the owner's server yet: list it again before
  sealing a name under it.
