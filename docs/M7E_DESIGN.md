# M7e design proposal — federation

Status: **accepted 2026-11-10** (§9). Step 5 of [M7_DESIGN.md](M7_DESIGN.md) (§7, §9), the
last of M7. Written after reading the current OCM specification,
**draft-ietf-ocm-open-cloud-mesh-07** (September 2026; the IETF working
group plans to ask for publication in December 2026), whose discovery
reports `apiVersion` 1.x.

## 1. What OCM gives, and what it does not

- **OCM is the notification protocol**: discovery (`/.well-known/ocm`),
  invitations (`/invite-accepted`), "you have been given access"
  (`POST /shares`), and share notifications (`/notifications`). It does not
  move bytes: the receiving server reaches the resource over **WebDAV**,
  with a bearer token (the shared secret, or a short-lived token from the
  **code flow** at the sender's `tokenEndPoint`). Requests between servers
  are signed with **HTTP Message Signatures** (RFC 9421, Ed25519, keys in
  a JWK Set at `jwksUri`).
- **Resource types** `file` and `folder`, share types `user` and `group`,
  WebDAV permissions `read`, `write`, `share`. **No roles, no whole
  drives, no end-to-end keys, no live changes**: those are our extensions
  (§6), used only between Organizer servers, which say so in discovery.
  Every other OCM server (Nextcloud, ownCloud Infinite Scale, CERNBox,
  Seafile …) gets plain OCM.

## 2. Turning it on

- **Settings → Federation** (superadmin): **off** (the default), **only
  the servers listed**, or **open** (with a list of servers refused). The
  page shows this server's address and its OCM state.
- **When on**, the server answers `/.well-known/ocm` (`enabled`,
  `apiVersion`, `endPoint`, `resourceTypes` file and folder with WebDAV
  both ways, `capabilities` `http-sig`, `invites`, `notifications`,
  `exchange-token`; `criteria` `must-use-http-sig`,
  `must-exchange-token`, and `allowlist` or `denylist` as set), WebFinger
  (`/.well-known/webfinger`, RFC 7033) for `acct:alice@host`, the OCM
  endpoints under `/ocm/`, and WebDAV for remote access under
  `/ocm/dav/<share>/`.
- **The server's signing key**: Ed25519, made at the first start in
  `<DATA_DIR>/ocm-key` (or `OCM_KEY_FILE`), published in the JWK Set.

## 3. People on other servers

- **Federated ids are `user@host`** (a drive is `drive@host`, as M7a
  prepared). A user finds someone by typing their address; WebFinger or
  the other server's OCM discovery confirms it.
- **Invitations**: "Invite someone on another server" makes an OCM invite
  (`token@host`, a link when the other side shows an accept dialog); when
  it is accepted, both get each other as **contacts**. **Sharing with
  someone on another server needs them as a contact** by default, unless
  their server is in the superadmin's list (that list then counts as
  trusted). **The superadmin can allow sharing with any address**
  (`federation.share_with_any`, off by default, superadmin and team
  level).
- **Contacts** live in Settings → Contacts: name, address, server, since;
  remove.

## 4. Sharing out

- **The Share dialog takes `user@host`** (a contact) like a local person.
  The role maps to OCM's permissions: viewer → `read`; editor → `read`,
  `write`; manager → `read`, `write`, `share`. **Upload only and owner are
  not offered for people on other servers** (OCM has nothing for them).
- **The share** goes to the other server as `POST /shares` (signed), with
  `resourceType` file or folder, protocol `multi` with `webdav` (a
  relative URI, a shared secret, `must-exchange-token`), and the owner's
  and sharer's federated ids.
- **Their server reaches the files** through `/ocm/dav/<share>/` with a
  token from our code flow; roles, limited access (M7b) and the drive's
  availability (M7c) hold there exactly as for a local user. Our WebDAV
  answers **sync-collection** (RFC 6578) there, so they can follow
  changes without walking the tree.
- **Removing them or changing the role** sends `SHARE_UNSHARED` or
  `SHARE_CHANGE_PERMISSION`. A drive's managers can forbid sharing with
  other servers for their drive (a drive-level setting,
  `federation.allowed`), and the superadmin at any level.
- **Links (M6b) are unchanged**: anyone with a link can already open it
  from anywhere.

## 5. Sharing in

- **A share from another server** arrives in **Shared with me**, marked
  with its server ("from alice@cloud.example"), and can be accepted,
  declined (`SHARE_ACCEPTED`, `SHARE_DECLINED`) or placed in My drive as
  a folder (M6b's mounts).
- **Bytes always go through the user's home server** (M7 §7): the web,
  the desktop and the apps ask our server, which reads and writes the
  other server over WebDAV with its token. The clients need nothing new:
  a remote folder is a folder, with `my_role` from the permissions.
- **Changes**: our server follows the remote folder with sync-collection
  when the other server offers it, else by comparing ETags on a schedule
  (every few minutes, sooner while someone has it open), and writes the
  changes into the user's change feed, so sync and the apps follow them.
- **Search and AI** may read remote shares as local ones; the superadmin
  can turn that off for this server (`federation.read_remote`, on by
  default). No flag asks the other server to restrict anything.
- **Limits**: a remote share counts against nothing here; writes count on
  the server that holds the files.

## 6. Between Organizer servers (our extensions)

Advertised in discovery as `capabilities` `organizer-drives`,
`organizer-e2e`, `organizer-live`; used only when both sides have them.

- **Whole drives** (`organizer-drives`): a shared drive's member on
  another server gets the **drive** (`drive@host`) in their sidebar with
  their role (all five except owner), not a folder.
- **Encrypted folders** (`organizer-e2e`): the member's public key comes
  from their server; the owner's device wraps the folder keys for them as
  for a local member (E2E_SPEC §4), and the wrapped keys stay on the
  owner's server. Encrypted folders are **never** shared with servers
  without this capability.
- **Live changes** (`organizer-live`): the holding server notifies the
  member's server of changes at once (a signed notification), so remote
  drives update as fast as local ones; without it, §5's polling.

## 7. The API (outline)

Public, for other servers: `/.well-known/ocm`, `/.well-known/webfinger`,
`/ocm/shares`, `/ocm/notifications`, `/ocm/invite-accepted`,
`/ocm/token`, `/ocm/jwks`, `/ocm/dav/…`. For our clients:
`/api/contacts` (list, remove), `/api/invites/federated` (make, accept),
the Share dialog's grants taking `user@host`, `Drive.remote` and
`Node.remote` (host), and `/api/superadmin/federation` (mode, lists).

## 8. Pages (mockups)

1. Settings → Federation (superadmin).
2. Settings → Contacts, with "Invite someone on another server" and
   accepting an invite.
3. The Share dialog with a person on another server.
4. Shared with me with a share from another server, and a remote drive in
   the sidebar.

## 9. Decided (2026-11-10)

1. **The design** and **the pages** as proposed and mocked up.
2. **`httpsig 0.0.26`** and **`ed25519-dalek 3.0.0`** as direct
   dependencies of the backend.
3. **Contacts first** by default, and **any address when the superadmin
   allows it** (`federation.share_with_any`, §3).
4. **Testing**: two Organizer servers in the integration tests, and a
   **Nextcloud container** in a manual mise task (podman), not in CI.

## 10. Built (backend)

Built 2026-11-10 against draft-ietf-ocm-open-cloud-mesh-07 (and RFC 9421,
RFC 9530, RFC 6578, RFC 7033, RFC 6749 §4.1). The rules as built are in
`apps/server/AGENTS.md` (**Federation**); the choices made while
building, then every place where the OCM text is not followed word for
word.

### Choices

1. **People on other servers are not users here.** A share with
   `user@host` is an `ocm_shares` row (its id is OCM's `providerId`), not a
   grant: `grants.user_id` is a user, and making shadow users would have
   put them in every list of users. What its recipient may do is
   `outgoing::role_in`, the grant rules again for one share (limited access
   below its top cuts it, the giver's role caps it, a whole drive is cut by
   any limited folder, upload only sees what it added, nothing encrypted
   without `organizer-e2e`). The Share dialog's endpoints take it as a
   grant: `NewGrant.address`, `Grant.remote` (address, name, server,
   `state` pending, accepted, declined), changed and removed by its id at
   the grant endpoints.
2. **A share from another server is a drive** (`kind` `remote`, migration
   0033 makes the table again for the kind), whose storage is that
   server's WebDAV through opendal's WebDAV service (already built in
   through rustic_backend: a feature, no new crate) on our guarded client
   with the share's bearer token. Its recipient has a grant on its root
   (a whole drive: on the drive), so "Shared with me", placing (M6b's
   mounts), the files API, `my_role`, sync and the change feed are the
   ones every share has. On such a storage a delete is the other
   server's (it keeps its own trash; ours returns a `TrashItem` that
   cannot be restored), no version is kept, nothing is hashed (it would
   download everything) and nothing counts against a limit. A shared file
   is the drive's one file (`Dav.single`).
3. **Addresses.** A user's OCM address is their email at our host
   (`alice@example.com@files.example.com`: OCM splits at the last `@`).
   Federation needs `PUBLIC_URL` (our host, `endPoint`, `jwksUri`, the
   signature's `@target-uri`); without it the mode is `off` whatever is
   stored (`FederationSettings.effective_mode`).
4. **Keys and signatures.** Ed25519 (`ed25519-dalek` makes the public key,
   httpsig builds and checks the signature base); the key id is
   `<host>#<16 hex of the key's SHA-256>`, so a new key has a new id and
   other servers fetch our JWK Set again. Freshness: `created` at most 5
   minutes old, 60 seconds in the future. Content-Digest sha-256 (sha-512
   is checked too when sent).
5. **The code flow.** Our tokens live 300 seconds, in memory only (a
   restart drops them: the other server exchanges again on the 401). The
   token request's `client_id` must be the share's recipient's server.
   Ours to other servers are renewed 20 seconds before they end, or at
   once on a 401 (`backends::Bearer`).
6. **sync-collection** on our WebDAV: the sync-token is
   `<PUBLIC_URL>/ocm/sync/<share>/<seq>` over the share's change log
   (`ocm_changes`, written by `record_change` in the change's transaction;
   `Change.from_path` gives a move's old path); members only, level `1` or
   `infinite`; each member with `getetag`, `getlastmodified`,
   `resourcetype` and `getcontentlength` whatever the request's `prop`
   asks; a removed member (deleted, moved out, hidden by limited access)
   is a 404 response. A token older than what is kept (30 days, as the
   feed) is `valid-sync-token` (403). A REPORT on a shared file is
   `supported-report` (403).
7. **Following an incoming share**: sync-collection on a folder when the
   other server answers it (remembered per share), else its ETag
   (`getetag` of the root, as Nextcloud changes a folder's ETag with what
   is in it); a change found is a scan of the drive (the reconciler of
   every drive, so the change feed follows). Every 5 minutes, every 30
   seconds while someone opened it in the last 10 minutes, and at once on
   a live notice.
8. **Our extensions** ride in OCM's own messages: an `organizer` object in
   the Share Creation Notification (`role`, `drive`, `e2e`) and in a
   notification (`role`), read only from a server that advertises them;
   the live notice is a notification of type `ORGANIZER_CHANGED`, sent
   only to servers with `organizer-live`. A whole drive is OCM's `folder`.
9. **Contacts first both ways**: a share arrives only from a contact of
   its recipient, from a listed server, or when the recipient's
   `federation.share_with_any` allows anyone (OCM §7.3 lets a receiver
   discard shares from unknown parties).
10. **Notifications are sent at once**, in the request that changes the
    share, and not retried: OCM lets a sender revoke without telling, and
    the tokens stop anyway. A shared folder trashed ends its shares (as it
    ends grants) and tells their servers, in the background.
11. **`federation.allowed`** (superadmin, team, drive; default on) stops a
    drive's existing shares too (token and WebDAV refused), as
    `mirrors.allowed` stops its mirrors. Turning the mode off or refusing
    a server does the same, until it is allowed again.
12. **The API** for the clients (tags Federation, Sharing, Superadmin):
    `GET /contacts`, `DELETE /contacts/{id}`; `GET/POST /invites/federated`,
    `DELETE /invites/federated/{id}`, `POST /invites/federated/accept`;
    `GET /federation/lookup?address=` (may the user share with it, why not,
    what its server takes); `POST /shared/{id}/accept` (declining and
    placing are `DELETE /shared/{id}` and `POST /shared/{id}/place`, which
    accepts too); `NewGrant.address`, `Grant.remote`, `SharedItem.remote`,
    `Node.remote` (host), `Drive.remote` and `DriveKind` `remote`;
    `GET/PUT /superadmin/federation`; the settings
    `federation.share_with_any`, `federation.allowed`,
    `federation.read_remote`. Refusals carry a `code`: `federation_off`,
    `server_refused`, `contact_required`, `federation_forbidden`,
    `e2e_unsupported`, `drives_unsupported`, `remote_unreachable`,
    `remote_refused`, `invite_invalid`, `invite_used`.
13. **What a share from another server is not**: shared again from here
    (no grant, link or OCM share of it: OCM §8.3, a receiving server never
    reshares; 400), a place the drop box files into, swept by our
    retention, indexed at start (it is followed instead), or mirrored.
14. **The Nextcloud check** is `scripts/interop-nextcloud.sh`
    (`mise run interop:nextcloud`, manual): `nextcloud:35.0.2-apache`,
    shares both ways, never run in CI (it pulls an image).
15. **The key exchange of `organizer-e2e`** (docs/E2E_SPEC.md §12;
    `federation/e2e.rs`, migration 0034). On the owner's server the
    member on another server is the share with them: listed among the
    folder's `members` with `remote` (their address) and `user_id` = the
    share's id, in `/encryption/pending`'s `waiting`, and given keys by
    `POST /encryption/folders/{id}/keys` with that id, kept in
    `e2e_remote_keys`. Their public key comes from their server
    (`POST /ocm/organizer-e2e/public-key`, signed, asked when the share is
    made and again at most once a minute while it has none; kept in
    `ocm_shares.public_key` once given: a key is never replaced). Their
    server asks for their keys (`POST /ocm/organizer-e2e/keys`, signed)
    whenever its user's device asks `/encryption/folders/{id}`, so the
    keys never stay there; it answers them with the owner's `folder_id`,
    no members and nothing to post. The owner's node ids reach its index
    through our WebDAV's sync-collection, which carries `o:id` (and the
    top's `o:root-id`, namespace `urn:x-organizer:dav`) to a share marked
    `e2e`: read after each scan (and when a folder is made through it) and
    kept as `Node.source_id`, so the clients open it with the mirror's code
    paths. A share that ends, is declined, no longer reaches the folder
    (`settle_e2e`) or whose user is removed takes its keys with it, and the
    folder `needs_new_key` (`e2e::remote_gone`). Both endpoints answer only
    a signed request from a server with `organizer-e2e`. The answers are
    not signed themselves: they come over the TLS of the server asked, as
    discovery and the JWK Set do.
16. **A push notice** (`push::Kind::Shared`, "Something was shared with
    you", as for a local share) goes to the recipient of a share that
    arrives from another server.
17. **`USER_REMOVED`** is sent, in the background and once, when a user is
    removed (`outgoing::removed_user`, `tell_removed`): to the servers of
    their contacts, of the shares they made or of their drive, and of
    those they received, with `notification.user.userId` their address.
    One received drops that contact, forgets what they shared with our
    users (owner or sender), and ends what our users shared with them
    (with no notification back).
18. **Uploads into a share from another server stay one PUT, read whole**:
    opendal's WebDAV writer (0.58) is a one-shot writer that keeps the
    bytes in memory and sends one `Buffer`, and WebDAV has no multipart or
    append; streaming would take a PUT of our own beside opendal. Said in
    `Storage::upload`.
19. **What a contact's server is** (migration 0035): `Contact.provider`
    (its discovery's `provider`, "Nextcloud"; null when it names none) and
    `Contact.organizer` (it advertises one of our extensions), kept when
    the contact is made and refreshed whenever that server's discovery is
    fetched again (every 5 minutes at most, by any use). Settings →
    Contacts says "jonas@cloud.example · Nextcloud · since 3 Nov".
20. **Following a share that fails**: a sync-collection REPORT that cannot
    be sent, or is refused (401, 403, 404, 5xx), is the share's `error`
    and is asked again next time; only 400, 405, 415, 501 or
    `supported-report` turn it off for the ETag. A listing that fails after
    a change is the share's `error` too. (Before, a server that went away
    turned sync-collection off for good and showed no error.)
21. **A sign-in goes on to `next`**, any path of this server (OCM's invite
    accept dialog `/settings/contacts?token=…&providerDomain=…` among
    them), after a second factor and single sign-on too; never to another
    origin (`allowed_next`, `allowedNext`).

### Checked in a browser (2026-11-10)

Two servers (`localhost:8301` and `127.0.0.1:8302`, a temporary
`DATA_DIR` each, `PUBLIC_URL`, `FEDERATION_ALLOW_PRIVATE=true`):

- Signed out, OCM's accept dialog link on B → the login page with `next`
  → password and a two-factor code → back at `/settings/contacts` with
  the accept dialog open; accepted. `//host`, `/\host` and absolute URLs
  are refused as `next` (tests: `allowed_next`, `allowedNext`,
  `single_sign_on_goes_on_to_the_page_it_came_from`).
- The Share dialog of a folder below one shared with bob on B: the
  inherited row has the server badge. The Share dialog of an encrypted
  folder shows "Encrypted folders can only be shared with Organizer
  servers."
- Settings → Contacts: "… · Organizer · since …", and a Nextcloud contact
  "… · Nextcloud · since 3 Nov" (its row written by hand: no Nextcloud
  here).
- Shared with me at 1024 px: Name 314 px wide (it was about 100 px); Role
  under the name.
- Decline on a pending share from A: gone on B, `declined` on A.
- A stopped: Shared with me on B says "localhost:8301 isn't reachable
  now: the server cannot be reached" (this found the follow bug, 20).
- Not tried: a whole remote drive's `RemoteDriveBanner` error (same
  `remote.error`), and a real Nextcloud (`mise run interop:nextcloud`).

### Where the OCM text is not followed word for word

1. **WebFinger's link relation**: OCM registers none; ours is the
   specification's URI (an extension relation type, RFC 8288), pointing at
   `/.well-known/ocm`. `acct:` URIs take the email's `@` as `%40` or plain.
2. **Legacy protocol forms are not taken**: `protocol.name` `webdav` with
   `options` (deprecated, Basic authentication) is answered 501; so are
   the `group` share type (we advertise `user` only), `REQUEST_RESHARE`
   and `REQUEST_SHARE` (optional), and webapp and ssh shares.
3. **`must-use-mfa`** in a share is refused (400): we do not advertise
   `enforce-mfa`.
4. **A share's absolute `uri` must be at the sender's server** (the host
   of its discovery's `endPoint`), else 400: otherwise a server could send
   our server, with a bearer token, wherever it likes.
5. **`expiration`** of an incoming share is not acted on (kept nowhere).
6. **Answers OCM leaves open**: the error bodies of our OCM endpoints are
   `{"message": "…"}` (the token endpoint's are OAuth's `{"error": …}` as
   §10.3 says); a notification taken is 201 with `{}`; an invite whose
   signature does not verify is 403 (as an untrusted server), a share's
   401 (§7.2); an unknown share in a notification is 404.
7. **`inviteAcceptDialog`** is `/settings/contacts` (a path, as §5.3 says),
   a page the web app must provide (with `token` and `providerDomain`).
8. **`ownerDisplayName` / `senderDisplayName`** (and an invite's `name`)
   are the user's email: users have no display names here. For an item of a shared drive
   (which has no owner) `owner` is the sharer.
9. **`webdav-receive`** says `{"uri": "relative"}`; absolute URIs are taken
   too.
10. **`USER_REMOVED`'s details**: ours carries the removed user's address
    as `notification.user.userId` (and `resourceType` `user`), the form we
    read; one from another server is acted on only for an address at its
    own host.
11. **`organizer-e2e`'s two endpoints** are ours, under the OCM
    `endPoint` (`/organizer-e2e/public-key`, `/organizer-e2e/keys`), not
    OCM's; only Organizer servers that advertise the capability call them.

### Not done

- A share from another server that is not placed and not a whole drive is
  not in the user's search (as a local share), and its changes reach the
  feed only once placed (as a local share's).
- The Nextcloud check is written, not run here (it pulls an image).
- The clients' part of `organizer-e2e` (docs/E2E_SPEC.md §12): wrapping
  for a member marked `remote`, and opening a remote drive's encrypted
  folder by its `source_id`s.
