# Bowerbird

A light, self-hosted file server whose **drop box** files things for you:
drop files, folders or links in, and a local AI proposes where they belong
in your folder tree (and a better name), for you to confirm. Besides the
drop box it is an ordinary file server: browse, upload, download, search,
and mount over WebDAV. One program, one SQLite file, your files as plain
files on disk.

> **Status: early development.** Nothing here is usable yet. The plan is in
> [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

> The [bowerbird](https://en.wikipedia.org/wiki/Bowerbird) is a collector:
> it keeps the tiny trinkets it finds organized, sorted by colour and size.

## Running it

The image is not published yet. To build and run it from a checkout (see
[CONTRIBUTING.md](CONTRIBUTING.md) for the tools):

```sh
mise run build:docker
podman run -d --name organizer -p 8000:8000 -v organizer-data:/data organizer
```

Then open <http://localhost:8000>.

## Configuration

Environment variables, for what is needed before the database opens.
Everything else will be set in the app.

| Variable       | Default                    | Meaning                                                                                                                              |
| -------------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `PORT`         | `8000`                     | The port the server listens on                                                                                                       |
| `DATA_DIR`     | `data` (`/data` in the image) | Organizer's own files: the database, the drop box, uploads, caches, shared drives                                                  |
| `FILES_DIR`    | —                          | The folder of the first superadmin's files (the account made at setup), when you set it here rather than on the setup page: an existing folder (a share that already holds files is fine), indexed where it is. It then cannot be changed in the settings. Every other user has their own folder. Unset: chosen at setup, by default a folder in `DATA_DIR` |
| `DATABASE_URL` | `sqlite:<DATA_DIR>/organizer.db` | The database (SQLite only for now). It must be on a local disk, not on an NFS or SMB share. Back it up with the data dir if you move it |
| `VECTOR_DATABASE_URL` | `turso:<DATA_DIR>/vectors.db` | Where the embeddings are kept (only with an embeddings model). The scheme names the engine; Turso (`turso:<path>`) is the only one so far. They are made again from your files, so they need no backup |
| `PUBLIC_URL`   | —                          | Where Organizer is reached from, e.g. `https://files.example.com`. Passkeys need it: a passkey belongs to its host |
| `TRUST_PROXY`  | `false`                    | Trust forwarded headers (`X-Forwarded-For`, ...): set it only behind a reverse proxy                                                  |
| `SWAGGER_UI`   | `false`                    | Serve the OpenAPI spec at `/api-docs/openapi.json` and Swagger UI at `/swagger-ui`                                                   |
| `RUST_LOG`     | `info`                     | What is logged                                                                                                                       |
| `TOTP_REQUIRED` | `false`                   | `true` makes two-factor authentication required: someone without it sets up an authenticator app at their next sign-in, and nobody can turn it off |
| `OIDC_ISSUER`  | —                          | Single sign-on: the provider's issuer URL (`https://`). With `OIDC_CLIENT_ID`, turns single sign-on on                               |
| `OIDC_CLIENT_ID` | —                        | Single sign-on: the client id registered with the provider                                                                           |
| `OIDC_CLIENT_SECRET` | —                    | Single sign-on: the client secret, if the provider gave one                                                                          |
| `OIDC_SCOPES`  | `openid email`             | Single sign-on: the scopes asked for, space separated                                                                                |
| `OIDC_LABEL`   | `Single sign-on`           | Single sign-on: what the login button says                                                                                           |
| `OIDC_REDIRECT_URL` | —                     | Single sign-on: the callback URL, when the one worked out from the request is not right (`https://…/api/auth/oidc/callback`)          |
| `OIDC_ONLY`    | `false`                    | `true` makes single sign-on the only way in: no password signs in and none can be set (the first account is still made with one). Invites are then accepted through single sign-on, and open sign-up means single sign-on makes accounts |
| `SIGNUP_OPEN`  | (the superadmin settings, off)  | `true` lets anyone who reaches the server create an account from the login page; `false` keeps sign-up closed. Set, it cannot be changed in Settings → Users |
| `OIDC_CREATE_USERS` | (the superadmin settings, off) | `true` makes single sign-on create an account for someone the provider signs in with a verified email no account has; `false` refuses them. Set, it cannot be changed in Settings → Users |
| `AI_PROVIDER`  | —                          | The model "from the environment", locked in Settings → AI and used as the default: the kind of API, `openai` (any OpenAI-compatible server: Ollama, llama.cpp, LM Studio, vLLM, OpenAI, …), `ollama`, `anthropic`, `gemini`, … With `AI_MODEL`, turns the drop box on |
| `AI_ENDPOINT`  | the provider's own         | That model's base URL, e.g. `http://localhost:11434/v1`                                                                             |
| `AI_API_KEY`   | —                          | That model's API key, if it needs one                                                                                                |
| `AI_MODEL`     | —                          | That model's name at the provider: a chat model, the default (it proposes where drop box items go). Set, the default is locked     |
| `AI_EMBEDDINGS_MODEL` | —                   | A locked embeddings model at the same provider, for semantic search and better proposals. Unset: the one chosen in Settings → AI, or none |
| `AI_EMBEDDINGS_ENDPOINT` | `AI_ENDPOINT`    | The base URL of `AI_EMBEDDINGS_MODEL`, when it is another server                                                                     |
| `AI_MODEL_IMAGES` | (Settings → AI: off)    | The registered name of the vision model that reads photos and images; locks the choice                                              |
| `AI_MODEL_SCANS` | (Settings → AI: off)     | The registered name of the vision model that reads scanned PDFs (pages without text); locks the choice                              |
| `AI_MODEL_AUDIO` | (Settings → AI: off)     | The registered name of the speech model (an OpenAI-compatible `/audio/transcriptions`) that transcribes recordings; locks the choice |
| `AI_MODEL_EMBEDDINGS` | (Settings → AI: off) | The registered name of the embeddings model; locks the choice (instead of `AI_EMBEDDINGS_MODEL`)                                   |
| `AI_CONCURRENCY` | `1`                      | How many requests to the models at once, all of them together (1 to 16)                                                              |
| `AI_TIMEOUT`   | `120`                      | The longest a request to a model may take, in seconds (1 to 1800)                                                                    |
| `LINKS_ALLOW_PRIVATE` | `false`             | `true` lets the drop box fetch links on private and local addresses (your own network, this machine). Off, a link is fetched only from a public address, so that users cannot make the server reach what only it can |
| `SECRET_KEY_FILE`     | `<DATA_DIR>/secret.key` (made at first start) | A file holding the server key, 32 random bytes, that encrypts the secrets of the storage backends (S3 keys). Unset, one is made in the data dir at the first start. Keep a copy with your backups: without it the stored secrets cannot be read, and each backend's secret must be given again. A file named here is never made: it must exist |
| `BACKENDS_ALLOW_PRIVATE` | `false`          | `true` lets users add their own S3 storage on private and local addresses (your own network, this machine) and over plain http. Off, a user's own storage must be https at a public address, so that users cannot make the server reach what only it can. The superadmin's storage backends may be anywhere |
| `OCM_KEY_FILE`        | `<DATA_DIR>/ocm-key` (made at first start) | A file holding the 32-byte seed of the Ed25519 key this server signs its requests to other servers with (federation, OCM), published in its JWK Set. Unset, one is made in the data dir at the first start. A new key gets a new key id: other servers fetch it again. A file named here is never made: it must exist |
| `FEDERATION_ALLOW_PRIVATE` | `false`      | `true` lets federation reach other servers at private and local addresses (your own network, this machine) and over plain http, as OCM allows for testing setups. Off, other servers are reached over https at public addresses only |
| `TRASH_DAYS`          | (each user's, default `30`) | Days before what is in the trash is deleted for good; `0` keeps it until the trash is emptied. Set, it is the same for everyone and cannot be changed in the settings |
| `VERSIONS_HOURS`, `VERSIONS_DAYS`, `VERSIONS_MONTHS` | (each user's, default `24`, `30`, `12`) | How long a file's earlier contents are kept: every one for so many hours, then one a day for so many days, then one a month for so many months; all `0` keeps none. Kept only for changes made through Organizer (web, apps, WebDAV), not for changes made directly in the files folder. Set, a rule is the same for everyone |
| `BACKUP_REPOSITORY`   | (the superadmin settings) | Where the backups go: an absolute path (another disk, a mounted share) or `s3://bucket/path`. With `BACKUP_PASSWORD`, backups run. See [Backups](#backups) |
| `BACKUP_PASSWORD`     | (the superadmin settings) | The repository's password: it encrypts the backups, and they cannot be read without it. Keep a copy away from this server |
| `BACKUP_S3_ENDPOINT`, `BACKUP_S3_REGION`, `BACKUP_S3_ACCESS_KEY`, `BACKUP_S3_SECRET_KEY` | (the superadmin settings) | For an `s3://` repository: the endpoint (empty for AWS), the region (default `us-east-1`) and the keys |
| `BACKUP_SCHEDULE`     | `daily 03:00`       | When backups run: `daily HH:MM` (the server's time zone) or `every Nh` |
| `BACKUP_KEEP`         | `7,4,12`            | Which backups are kept: the last of so many days, weeks and months (the newest always) |
| `VAPID_PRIVATE_KEY`   | (made at first start) | The key the server signs its Web Push requests with: base64url, its 32 raw bytes (as `npx web-push generate-vapid-keys` prints it). Unset, one is made and kept in the database. Changing it stops the notices of every browser and phone until they are turned on again. See [Notifications](#notifications) |
| `APNS_KEY`, `APNS_KEY_ID`, `APNS_TEAM_ID` | (none: no notices to the iPhone app) | Apple Push Notification service, for the iPhone app: the `.p8` key's contents (its line breaks may be written `\n`), its key id and your team id. All three, or none |
| `APNS_TOPIC`          | `eu.vardalabs.organizer` | The iPhone app's bundle id |
| `APNS_SANDBOX`        | `false`             | `true` sends to APNs' development environment (an app built and run from Xcode) |

Yes/no variables take `true`/`false`, `1`/`0`, `yes`/`no` or `on`/`off`; any
other value stops the server at start. An `OIDC_*`, `AI_*` or `BACKUP_*`
variable that is set wins over its field in the superadmin settings, which then
shows it as locked (as do `SIGNUP_OPEN` and `OIDC_CREATE_USERS`); `TRASH_DAYS`, `VERSIONS_*`,
`TOTP_REQUIRED` and `AI_MODEL_*` lock their [setting](#settings) for every
level (superadmin, teams, users, drives).

## Users

The account made at setup is the first superadmin. A superadmin invites others in
Settings → Users: an invite link (for one email, or anyone with the link,
and optionally as a superadmin) works once, for 7 days; Organizer sends no
email, you send the link. Open sign-up and single sign-on making accounts
are two switches there (`SIGNUP_OPEN`, `OIDC_CREATE_USERS`). Each user has
their own drop box and their **My drive**, their own files in their files
folder: by default `<DATA_DIR>/users/<id>/files`, or an existing folder the
superadmin sets. A superadmin can make or remove superadmins, set a storage limit (writes
through Organizer past it are refused; the trash and versions count, and
changes made directly in a files folder are counted, not stopped), disable
a user (they cannot sign in, and their sessions, API keys and devices stop
working until enabled again) or remove one (their account and Organizer's
data for them; their files folder is deleted only when it is the default
one in the data folder, never an existing folder). There is always one
superadmin who is not disabled.

The superadmin's pages (and their API, `/api/superadmin/…`) ask for a
passkey or a two-factor code again when the last one was more than 15
minutes ago; a superadmin with neither adds one first (Settings →
Sign-in). They take a signed-in session only: no API key
or device token reaches them. (`/api/admin/…`, their old path, still
answers until the next release.)

When nobody can reach them (the only superadmin lost their phone, or left),
run on the server, with the same `DATA_DIR` or `DATABASE_URL` (in the
image: `docker exec <container> organizer …`):

```sh
organizer superadmin grant <email>    # make a user a superadmin
organizer superadmin revoke <email>   # never the last one who is not disabled
organizer two-factor reset <email>    # remove the user's two-factor app and passkeys
```

A two-factor reset also signs the user out everywhere: they sign in with
their password or single sign-on, and set a second factor up again.

What a superadmin (or these commands, as "the server's command line") did
to an account — made it through an invite, disabled or enabled it, changed
its role, storage limit, files folder or teams, reset its two-factor
authentication, changed its access to a shared drive — is in that user's
**Settings → Security → Account activity**. A superadmin still sees only
the files they own or that are shared with them.

## Teams

A **team** has a name, members, its own **managers**, a storage limit for
all its shared drives together, and defaults for its members' settings. A
superadmin makes and removes teams in Settings → Teams and names their
first managers; a team's managers add and remove members and managers,
make the team's shared drives (whose managers they are, and whose members
are editors unless the team's role on the drive says otherwise) and set
the team's settings within what the superadmin left open. M6b's groups are
teams with their members (and no manager until the superadmin names one).

Someone in several teams gets a shared drive's settings from the drive's
team, the strictest of their teams' sign-in settings (one team requiring
two-factor authentication is enough), the loosest of their teams' locks and
ranges on their own settings, and the defaults of their **primary team**:
the first they joined, or the one they choose.

## Drives

A **drive** is a tree of files with its own trash, versions and storage
limit; the sidebar (and the apps' Files tab, and the desktop's Synced
folders) lists them: **My drive**, each **shared drive** you are a member
of, and **Shared with me**. A superadmin makes shared drives in Settings →
Drives: a name, its members, optionally a team, an existing folder on the
server (else a new one in `<DATA_DIR>/drives/<id>`) and a storage limit;
they see the drives there, not what is in them. By default a team's
managers make their team's drives too (the setting `drives.create`:
superadmin only, also team managers, or everyone). Who has a shared drive
and with what [role](#sharing) are its grants, to people and teams; its
**managers** decide who has access, rename it and set its settings, and a
shared drive always keeps one. A shared drive has no owner: deleting for
good from its trash is its managers'. Nothing moves between drives (copy
instead).

Each drive keeps its trash and versions in a hidden `.organizer` folder at
its root (on a server from before drives they are moved there from
`<DATA_DIR>/users/<id>/` at the first start). It is never listed, synced,
searched or shown over WebDAV, and no file or folder at a drive's root may
take that name.

### Storage

A drive lives in a **storage backend**: the server's disk (every drive so
far, and always My drive), or S3-compatible storage (AWS, Backblaze B2,
Cloudflare R2, MinIO, Garage, Scaleway, …). The superadmin adds global
backends in Settings → Storage: the endpoint, region, bucket, an optional
prefix and the keys (tested before they are kept; the secret is encrypted
with the server key, `SECRET_KEY_FILE`, and never shown again), who may make
drives on it (the superadmin only, team managers, everyone, or chosen
teams), a storage limit for all its drives and one for each new drive, and
how often its drives are listed for changes made outside Organizer (hourly
by default, or never). A drive on S3 keeps its files as plain objects at
`<prefix>/<drive id>/`, its trash and versions under `.organizer/` there;
everything works on it as on the disk. Moving or renaming a folder there
copies each object (S3 has no rename), so a large folder takes a while. A
shared drive can be moved to another backend: it is copied, read-only
meanwhile, and switched over when done.

When the superadmin allows it (the setting `storage.own_backends`, at
superadmin and team level), users add **their own** S3 storage (Settings →
Storage → Your storage), at a public https address unless
`BACKENDS_ALLOW_PRIVATE`, and make shared drives on it; its members see
whose storage it is, and the drive goes when its owner removes it. With
**private credentials** the secret is sealed to the owner's encryption key:
the server cannot read it, and the drive works only while a session of the
owner has unlocked it ("Locked until … signs in" otherwise).

A backend that fails (unreachable, credentials refused) makes its drives
unavailable, with the reason, until it answers again; nothing is deleted.
The server's backups cover only the drives on its disk: for S3, the
provider's versioning or replication is the way.

### Mirrors

A **mirror** is a one-way, read-only copy of a drive in your own storage
(Settings → Mirrors, or "Mirror…" on a drive's page): plain files at their
paths under `<prefix>/<mirror id>/` of the bucket, readable with any S3 tool
without Organizer. It copies what you see of the drive (not limited
folders you are not in, not others' uploads where you may only upload, not
the shares placed in your My drive); encrypted folders are copied as the
server holds them, sealed. A file deleted or replaced in the drive goes to
the mirror's own trash and versions, kept by your trash and version
settings, so a mirror also keeps what was deleted upstream for that long.
It copies **continuously** (within about a minute of a change), hourly,
daily, or only when you press **Mirror now**; on private credentials, only
while a session of yours has them unlocked, catching up after.

You mirror My drive whenever you have your own storage. Shared drives are
mirrored only once the superadmin (or the drive's team) turns on
`mirrors.shared_drives`, by a viewer or above (not upload only); a drive's
managers can forbid it for their drive (`mirrors.allowed`), which stops its
mirrors, and its access list shows who mirrors it. A mirror stays when its
drive is deleted or you lose access: it stops and says why, and keeps what
it has. Mirrors are listed with your drives, read-only: browse, download,
and restore from their trash or versions, which copies the file back into
the drive when you may write there (else download it). **Eject** makes a
mirror a drive of yours: in place (it becomes a shared drive where it is,
and stops mirroring) or as a copy (a new drive with its contents, and the
mirror goes on). Removing a mirror deletes its copy from the bucket. Mirrors
are not searched (their drives are).

## AI

The AI is a list of models, registered in Settings → AI (a name, the
provider, endpoint, key and model, and what it can do: chat, vision,
speech, embeddings), a default model, and the model of each kind of work:
the drop box's proposals (the default unless another is chosen),
embeddings, photos and images, scanned PDFs and audio (off until a model is
chosen). The `AI_*` variables of a connection are one more model, "from the
environment", locked. Photos (up to 20 MB, sent as they are), the pages of
scanned PDFs and recordings (their first hour) are read only as each
user's "read contents" setting and sensitive-file rules allow, once per
content; "Read existing files again" reads what was there before a model
was chosen.

## Sharing

**Share…** in a file's or folder's menu gives people or teams a **role**
on it (a grant to a team follows its members), the same roles as on a
shared drive:

| Role | May |
| --- | --- |
| owner | everything, including deleting for good and handing it over (My drive only) |
| manager | who has access, the item's settings; all an editor may |
| editor | read, write, rename, move, delete (to the trash), make links |
| viewer | read and download |
| upload only | add files, and see only their own |

A grant reaches everything under it; a subfolder can give more (another
grant, a higher role) or turn on **limited access**: from there down only
the grants made on it or below count, and everyone else does not see it
at all — not in listings, search, the trash, versions, sync, WebDAV or the
drop box. The owner and the drive's managers always keep access. Someone
gives at most their own role, never owner; the owner **hands an item over**
to someone else, who becomes its owner while they become its manager (it
stays where it is). A grant made by a manager who is not the owner gives no
more than they still have. Every file and folder the API returns says the
signed-in user's role (`my_role`), so the apps offer only what it allows.
(M6b's shares became grants: read only a viewer, with create, change or
delete an editor, with re-share a manager.) What is shared with you is in **Shared with
me** (the sidebar's badge counts the new ones): **Add to my files…** puts
it as a folder anywhere in your own files, where it is synced, in WebDAV
and in the apps like your own; **Remove from my files** takes it out again
(it stays shared with you), and **Decline** removes it for you. Storage,
versions and trash stay the drive's: what you delete in a shared folder
goes to its drive's trash, and what you add counts against that drive's
storage limit. Shares work inside any drive (in a shared drive, made by
who manages it); added to your files, they go into My drive. The drop box
may file things into My drive, the shared drives you are a member of, and
a shared folder you added to your files and may create in.

The same dialog makes **public links** (`/s/<token>`, no account needed),
for editors and above, as the settings `links.allowed`,
`links.password_required` and `links.max_days` allow:
**view** (the file, or the folder's listing with downloads and the folder
as a zip), **edit** (also upload, new folder, rename and delete in it) or
**file drop** (upload only, nothing of the folder shown); each with an
optional password, asked once per browser session (wrong ones count
against the sign-in limits), and an optional expiry. A link stops working
when it expires, while its owner is disabled, when links are turned off
where it is, and when its file or folder is deleted (deleting a shared
folder, to the trash too, ends its grants and links; what was limited stays
limited when it is restored).

## Federation

People on other servers — another Organizer, Nextcloud, ownCloud Infinite
Scale, CERNBox, Seafile and the like — can be shared with through **Open
Cloud Mesh** (OCM, draft-ietf-ocm-open-cloud-mesh-07). It is **off** by
default; in **Settings → Federation** the superadmin chooses **only the
servers listed** or **open** (with a list of servers refused). It needs
`PUBLIC_URL`: other servers reach this one there, and its users' addresses
are `<email>@<host>` (`alice@example.com@files.example.com`).

- **Contacts.** **Invite someone on another server** (Settings → Contacts)
  makes an invite to send them (a code to paste on their server, and a
  link); once they accept it on their server, each is the other's contact.
  An invite from someone elsewhere is accepted there too (**Accept an
  invite**, or their server's link to `/settings/contacts?token=…`, which
  opens the accept dialog). By default people share with their contacts only, and take
  shares from them only; anyone on a server the superadmin **listed**
  counts as trusted, and the setting `federation.share_with_any`
  (superadmin, team) allows any address.
- **Sharing out.** The Share dialog takes `user@host` like a local person
  (it asks their server first, and says why not when it cannot):
  viewer, editor or manager (OCM's `read`; `read`, `write`; and `share`).
  Their server gets the share (signed) and reaches the files through this
  server's WebDAV for it, with short-lived tokens; roles, limited access
  and the drive's storage hold there as for anyone here. Changing their
  role or removing them is told to their server. A drive's managers may
  forbid sharing it with other servers (`federation.allowed`).
- **Sharing in.** A share from another server arrives in **Shared with
  me**, marked with its server; accept it, decline it, or add it to your
  files. Its files stay on that server: the web, the apps and sync reach
  them through this server, with your role there. Its changes are followed
  (every few minutes, sooner while you have it open) into your change feed.
  Search and the AI read them too unless the superadmin turns
  `federation.read_remote` off. A remote share counts against no storage
  limit here. It is never shared again from here (no Share, links or
  mirrors of it), and the drop box never files into it.
- **Between Organizer servers**: whole shared drives (with any role but
  owner, in the sidebar's "Other servers", accepted or declined at the
  drive's top), encrypted folders (never shared
  with a server that cannot keep them sealed; they arrive sealed, and
  their keys for members elsewhere are not exchanged yet, so they stay
  locked there for now) and changes told at once.

Requests between servers are signed (HTTP Message Signatures, Ed25519;
the key is `OCM_KEY_FILE` or made in the data dir); unsigned ones are
refused. `FEDERATION_ALLOW_PRIVATE` lets it reach servers on your own
network or over http, for testing. `mise run interop:nextcloud` (manual)
tries sharing both ways with a Nextcloud container.

## Settings

Some settings have **levels**: the superadmin's, a team's, a user's own and
a drive's, under the environment variables, which lock what they set. At
each level a setting is **locked** (fixed for everyone below, who see it
disabled with "Set by your administrator" or "Set by <team>"), a
**default** (those below may change it), a **range** (a number those below
choose within, such as "the trash keeps at least 7 days") or **not set**.

| Setting | Levels |
| --- | --- |
| Drop box: pending mode, reading contents, folder freedom | superadmin, team, user |
| Trash and versions | superadmin, team, user, drive |
| Notifications | superadmin, team, user |
| AI: the model of each kind of work, among the superadmin's | superadmin, team, user |
| AI: users may add their own models | superadmin |
| Public links: allowed, password required, longest life | superadmin, team, drive |
| Encrypted folders allowed | superadmin, team |
| Two-factor authentication required | superadmin, team |
| Who may make shared drives | superadmin |
| Users may add their own storage | superadmin, team |
| Shared drives may be mirrored (`mirrors.shared_drives`) | superadmin, team |
| This drive may be mirrored (`mirrors.allowed`) | drive |

The rest (backups, single sign-on, sign-up, the AI models and their
concurrency) stay the superadmin's alone.

## Encrypted folders

Folders whose names and contents only you and the people you share them
with can read: not the server, its superadmins, or its backups
([docs/E2E_SPEC.md](docs/E2E_SPEC.md) is the format).

1. **Settings → Encryption → Set up encryption**: choose an encryption
   passphrase (not your sign-in password) and type it twice. Organizer
   then shows your **recovery key** once: copy it or download it, keep it
   somewhere safe, switch on "I have saved it", Done.
2. **New folder → Encrypt this folder**, in your own files. It shows a
   lock. On a device where you have not unlocked yet, it says "Unlock to
   see what's inside": enter your passphrase there (the web keeps the key
   until you lock it or reload the page; the apps and the desktop keep it
   in the system's secure storage until you lock or sign out).
3. Inside, everything works as in any folder (upload, open, rename, move,
   delete, the trash and versions), decrypted and encrypted on your
   device. The folder's own name stays readable; what is in it does not.
4. **Share…** it with people or groups as any folder. Someone who has not
   set up encryption yet shows as "Waiting for them to set up encryption"
   and gets access once they have and your device next opens it. Removing
   someone gives the folder a new key for what is written after.

Not for encrypted folders: the AI drop box (it never proposes, reads or
files into them), search (not even their names), public links, WebDAV
(they are hidden there), and the desktop app's on-demand folders (an
encrypted folder syncs in full; one inside an on-demand folder is left
out of it). Forgot the passphrase? **Settings → Encryption →
Use the recovery key…** sets a new one. **If you lose the passphrase and
the recovery key, nobody can read them**: not you, not a superadmin, not us.

**Decrypting by hand.** `organizer-decrypt` (`mise run build:decrypt`)
decrypts an encrypted folder with your passphrase or recovery key, so you
never depend on the apps to read your files: from a running server with
an API key (`--server https://… --api-key org_… --folder /Private --out
./Private`), or from a restored backup without any server (`--data-dir
/restore/data --user me@example.com --folder /Private --out ./Private`).
The steps are in [docs/E2E_SPEC.md §10](docs/E2E_SPEC.md#10-decrypting-by-hand-organizer-decrypt).

## Notifications

In Settings → Notifications each user turns notices on in a browser (the
browser asks for permission; on an iPhone or iPad, add Organizer to the home
screen first), picks what they are told about (items waiting for review in
the drop box, at most one notice a quarter hour, with the count; something
shared with them; a failed backup, for superadmins), and sees their devices with
when each last got a notice. The apps register themselves: Android through
[UnifiedPush](https://unifiedpush.org) (install a distributor such as ntfy),
the iPhone app through Apple's push service once the `APNS_*` variables are
set (until then it is listed as "Notices not available yet").

A notice is a wake-up with nothing in it: the push services (Google,
Mozilla, Apple, ntfy) never see what it is about. The browser or app then
asks this server what is new and shows that. The push services see the
server's public key and `PUBLIC_URL` (set it: Safari needs it). A push
service's address must be https on a public address, unless
`LINKS_ALLOW_PRIVATE` is set (a UnifiedPush distributor on your own
network).

## WebDAV

Mount `https://<your server>/dav/` in Finder, Explorer, DAVx5, rclone or any
WebDAV client, with any user name and an app password (made in the settings,
or with `POST /api/auth/api-keys` and `"kind": "app_password"`). Deleting a
file over WebDAV puts it in the trash.

The drop box is a drive of its own: mount `https://<your server>/dav-inbox/`
the same way (it shows as "Organizer drop box"). A file you put there is
filed like one dropped in the app: at its top by your drop box settings, in
`review/` always waiting for you to confirm, in `auto/` placed at once. It
stays visible there until it is placed. A folder you copy there is one
item, placed whole, a few seconds after its last file came in.

Encrypted folders are not in WebDAV: a WebDAV client cannot decrypt them.

HTTPS is the reverse proxy's job (Caddy, Traefik, nginx, ...): Organizer
speaks plain HTTP.

## Backups

Organizer backs up the whole server: the database, every drive on the
server's disk (each user's files folder and each shared drive, with
their trash and versions), and the drop box in the data dir. Drives in an
S3 [storage backend](#storage) are not in it, and neither is the server
key (`<DATA_DIR>/secret.key`, or `SECRET_KEY_FILE`): keep a copy of it
apart, or give the backends' secrets again after a restore. Set them up in
Settings → Backups, or with the `BACKUP_*` variables. The backups are
[restic](https://restic.net)-format repositories made with
[rustic](https://rustic.cli.rs): deduplicated, compressed and encrypted
with the password, on a local path or S3. Each user can restore a file or
folder of their own from a backup (a folder's menu → From a backup…); it
comes back next to the current one, never over it.

To restore everything, stop the server and run, with the same `DATA_DIR`,
`DATABASE_URL` and `BACKUP_*` variables:

```sh
organizer backup snapshots
organizer backup restore [--snapshot <id>] [--to <dir>] [--force]
```

The folders go back where they were (under `--to` first, to look), then the
database. A database that is there is replaced only with `--force`. In the
image: `docker run --rm -v ... <image> backup restore`. The `restic`
command reads the same repository: `restic -r <repository> snapshots`.

## iOS app

A native iPhone and iPad app (iOS 18 or later), in `apps/ios`: browse
and upload your files, review the drop box, and **share** files, photos
and links to Organizer from any app, to the drop box (with a note) or into
a folder you pick. You sign in once with your server's address, on your
server's own sign-in page; the app then shows under Settings → Devices,
where you can revoke it, and signing out in the app revokes it too. Large files go up in the background and continue
where they stopped after a dropped connection. Once signed in it asks to
show notifications (see [Notifications](#notifications)).

It is not in the App Store yet: build it for the simulator with
`mise run build:ios` (Xcode on a Mac; see [AGENTS.md](AGENTS.md)).

## Android app

`apps/android`: a native app (Android 10 and later) to browse your
files, review the drop box, and **share into Organizer from any app**:
files, photos, text and links go to the drop box (with a note and the
per-item options your drop-box settings allow) or to a folder you pick.
Uploads run in the background and resume where they stopped; large files
show their progress in a notification.

You sign in with the server's address, then in the browser as on the web
(password, single sign-on, two-factor). The app gets a token of its own,
listed under Settings → Devices, where you can revoke it. Signing out in
the app revokes the token too. Notices come through your UnifiedPush
distributor (a switch in the app's settings). Everything else is set on
the web.

It is not on Google Play yet. To build a debug APK and install it on a
device or emulator (`adb`): `mise run build:android`, then
`adb install apps/android/app/build/outputs/apk/debug/app-debug.apk`.

## Desktop app

`apps/desktop`: an app for macOS, Windows and Linux that keeps folders
of your server **in sync with your computer, both ways**, and lives in the
tray (the menu bar on macOS). You sign in with the server's address, then
in your browser as on the web; the app's token is kept in the system's
credential store (Keychain, Credential Manager, Secret Service), shows
under Settings → Devices, and is revoked when you sign out.

- **Synced folders:** pick a folder of the server (or all of it) and where
  it goes, by default `~/Organizer/<folder>`. A change on either side is
  made on the other: new files, edits, renames, moves and deletes. Deleted
  files go to the trash on both sides, never removed for good.
- **Conflicts:** a file changed on both sides is kept twice: the server's
  version in place, yours beside it as
  `Report (conflict, MacBook, 2026-11-07).docx`.
- **The Drop box folder** (`~/Organizer/Drop box`, can be turned off):
  what you put there is sent to the drop box and removed once the server
  has it; the AI's proposals show in the app's Drop box tab and the tray
  ("Review the drop box (3)").
- **Ignored files:** `.DS_Store`, `Thumbs.db`, `desktop.ini`, office lock
  and temporary files, browsers' partial downloads; the list can be edited
  in Settings. Names your system does not allow (`:` on Windows, names that
  differ only in case on macOS and Windows) get a safe local name, and
  renaming it renames the original.
- **On-demand folders** (macOS and Windows; Linux syncs in full): when
  you add a folder, choose **On-demand** rather than Sync. Its files show
  with their name and size, with a cloud icon, but download only when you
  open them, and then stay; **Free up space** and **Always keep on this
  device** (the system's own menu items on a file or folder) free one again
  or keep it downloaded. Changes, conflicts and deletes work as in a synced
  folder. On macOS the folder is in Finder's sidebar under Locations; on
  Windows it is at the place you chose, with its own entry in Explorer's
  navigation pane.
- Pause and resume from the tray or the window; changes made while the
  server cannot be reached go once it is back. Start at login is a switch
  in Settings.

There are no signed installers yet. Each release has the unsigned bundles
(`.dmg` for Apple silicon Macs, `.msi` and `.exe`, `.deb`, `.rpm` and
`.AppImage`), or build this system's into `dist/desktop` with
`mise run build:desktop` (see [AGENTS.md](AGENTS.md)). On macOS an unsigned app opens with right-click →
Open the first time.

**On-demand folders on macOS need a signed app.** macOS loads the app's
File Provider extension only when the app is signed with an Apple team:
until then, adding an on-demand folder says so. To build one that works on
your own Mac, set your team's id as `DEVELOPMENT_TEAM` in
`apps/desktop/macos/project.yml` (a free Apple account's Personal Team
is enough; a paid Developer ID, notarised, to give the app to others), set
`CODE_SIGN_STYLE` to `Automatic` and `CODE_SIGN_IDENTITY` to
`Apple Development` there, and sign the app with the same team
(`bundle.macOS.signingIdentity` in `src-tauri/tauri.conf.json`). Windows
needs no signing for its on-demand folders.

## License

[Hippocratic License 3.0](LICENSE) with the modules listed in it: source
available and free to use, with ethical limits. It is not an OSI-approved
open source license.
