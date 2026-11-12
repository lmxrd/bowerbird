# AGENTS.md

The web app: a client-rendered React SPA (Vite, Tailwind CSS 4, TanStack
Router and Query, stock shadcn/ui on Base UI), embedded in the backend's binary. The root
AGENTS.md has the rules of the whole repository; these are the frontend's.

## Commands

Run them through mise from the repository root (`mise run check:frontend`,
`test:frontend`, `dev:frontend`): those regenerate the SDK first. Inside this
folder:

- `pnpm dev` — dev server at http://localhost:5173 (`/api` goes to :8000)
- `pnpm build` — production build into `dist/`
- `pnpm test` — the Vitest unit tests
- `pnpm check` — oxfmt --check, oxlint and tsc (read-only; must pass)
- `pnpm fix` — oxfmt and oxlint --fix

## Before you build a page

**Present each page to the user and get their approval before creating
it** (what it shows, its layout, the shadcn components and blocks it uses),
as a visual mockup, not only a description. The app's layout is shadcn's
`sidebar-07` block ("a sidebar that collapses to icons"): `AppSidebar` in
`src/components/app-sidebar.tsx`, around every signed-in page
(`src/routes/_app.tsx`); each page starts with `PageHeader` (the sidebar's
button and the breadcrumbs).

The M1 pages (approved 2026-11-06): login, setup, files (`/` and
`/files/<path>`), trash, and settings (account, two-factor, devices and
passwords with the device approval at `/settings/devices/approve`, files
folder and single sign-on for superadmins).

The M2 pages (approved 2026-11-06): the drop box (`/drop-box`, in the
sidebar with a badge counting the items that wait for the user), search
(the field in every `PageHeader`, results at `/search?q=`), and settings
(drop box for every user, AI connection for superadmins; both end with "What
Organizer sends to the AI", `src/components/ai-recipient.tsx`).

The M6a pages (approved 2026-11-08): Settings → Users for superadmins (the
joining switches, the users with their menu, the invite dialog, pending
invites), the invite page (`/invite/<token>`), open sign-up (`/signup`, and
"Create an account" on the login page), and the user's storage in Settings
→ Account. The pages before sign-in share `AuthCard`, `SsoButton` and
`JoinOptions`.

The M6c page (approved 2026-11-08): Settings → AI (superadmin): the Models card
(Add a model, a row per model with its capabilities and a menu: Make
default, Edit, Test, Remove; the locked model of the `AI_*` variables), the
Work card (the model of each kind of work, Read existing files again) and
the Add a model dialog (`src/routes/_app/settings/ai.tsx`, its logic in
`src/lib/ai.ts`).

The M6b pages (approved 2026-11-08): the Share dialog
(`share-dialog.tsx`: people and groups with their permission toggles,
`RecipientIcon`; its Links section, `links-section.tsx`), "Shared with me"
(`/shared`, in the sidebar with a badge of the new ones; "Add to my
files…" picks a folder with `FolderBrowser ownOnly`), the Groups card in
Settings → Users (`groups-card.tsx`), and the public link pages
(`/s/<token>`, no sign-in: `__root.tsx` lets `/s/` through for anyone;
password, a file, a folder with an edit link's actions, a file drop). A
folder's `Entry.permissions` say what the user may do in it (a share's, or
everything in their own files): the folder view disables what it does not
give; `Children.mounts` are the user's placed shares (Delete is "Remove
from my files"), `Children.shared` what they shared. Link pages' bytes go
through `src/lib/transfer.ts` (`linkDownloadUrl`, `linkZipUrl`,
`uploadLinkFile`), the rest through the SDK; the pure logic is
`src/lib/sharing.ts`.

The M6d page (approved 2026-11-08): Settings → Notifications
(`src/routes/_app/settings/notifications.tsx`, its logic in `src/lib/push.ts`):
the card "This browser" (Turn on / Turn off), "Notify me about" (a switch per
kind) and "Devices". The service worker is `public/sw.js`, plain JavaScript
copied as it is to the root of the build: on a push it fetches
`/api/notifications/news` and shows the newest notice, and a click opens its
page.

The M6e pages (approved 2026-11-08): Settings → Encryption
(`src/routes/_app/settings/encryption.tsx`: set up with the passphrase
twice, the recovery key shown once with Copy, Download and "I have saved
it"; then this device's Unlock/Lock, Change passphrase, Use the recovery
key; the dialogs in `encryption-dialogs.tsx`), the New folder dialog's
"Encrypt this folder" switch, a locked folder's empty state ("Unlock to see
what's inside"), lock icons (`KindIcon encrypted`), and the Share dialog of
an encrypted folder (waiting members, no links). The format is
`src/lib/e2e.ts` (WebCrypto; Vitest checks `packages/e2e-crypto/vectors.json`),
the session's keys and the device's duties `src/lib/encryption.ts` (the
private key a non-extractable CryptoKey, in memory only: a reload locks),
encrypted uploads `uploadEncryptedFile` in `transfer.ts`. A decrypted file
is shown in a tab only for types that cannot run as a page
(`inlineType`); anything else is saved.

The M7a step-up dialog (approved 2026-11-09; docs/M7A_DESIGN.md §4,
`src/components/step-up.tsx`, its logic in `src/lib/step-up.ts`): "Confirm
it's you", ending "You won't be asked for 15 minutes.", a "Use passkey"
button (when the user has one and passkeys are on), a separator "or", the
code field (six digits), Cancel / Confirm; for a superadmin with neither,
"Add a passkey or two-factor first" with a link to Settings → Security.
Every page that calls a superadmin endpoint (Users with its Groups card and
invite dialog, Files folder, Single sign-on, AI, Backups) is wrapped in
`SuperadminGate` in its route (a dialog, then the page; the retention
defaults wait for a click, `lazy`), and any superadmin request refused with
`step_up_required` opens the dialog and is sent again once confirmed
(`installStepUp`, an interceptor of the SDK's client, in `main.tsx`).
`StepUpProvider` (in `_app.tsx`) holds the one dialog. A new superadmin
page goes in a `SuperadminGate` too.

The M7a pages (approved 2026-11-09; docs/M7A_DESIGN.md §6): the sidebar's
"Drives" group (`app-sidebar.tsx`): My drive, each shared drive (from
`listDrives`, fetched again with every live `changes` event), then "Shared
with me" with its badge; the drop box, trash and settings below as before.
A drive's page is the one folder view (`FolderView`, given the `drive` whose
page it is): My drive stays at `/` and `/files/<path>`, a shared drive is
at `/drives/<id>` and `/drives/<id>/<path>` (`routes/_app/drives/$driveId/`,
`SharedDrivePage` in `drive-page.tsx`, which walks the path down from the
drive's root: `/files/by-path` only knows My drive). The crumbs start at
the drive, its root is titled with its name, and a shared drive's root
says "N members · size" with "Members…" (and Rename and Settings for who
manages it).
"Members…" is M7b's drive access dialog (below); the rename dialogs are
`drive-dialogs.tsx`. The trash is a drive's
(`/trash?drive=<id>`, My drive's without it, a select between them; the
sidebar's link carries the shared drive the user is in). Settings → Drives
(superadmin, `routes/_app/settings/drives.index.tsx`): the "Shared drives" card,
"New shared drive" (name, an existing folder on the server, a storage
limit, its team, members with "Manages"), a row per drive (name, "managed by …",
members, "used" or "used of limit") and its menu: Members…, Rename,
Storage limit…, Delete… (typed name to confirm). Moves stay in the drive
(the Move dialog's path is from the drive's top), search hits open in
their drive, backups are offered in My drive only, and "Add to my files…"
still picks a folder of My drive. URLs, "N members · size" and the like
are `src/lib/drives.ts`; looking folders up in a drive `src/lib/drive-files.ts`.

The M7b pages (approved 2026-11-09; docs/M7B_DESIGN.md §9): the Share
dialog with roles (`share-dialog.tsx`, replacing M6b's: "Add people or
teams" with a role select, the owner, direct grants with their role and
remove, inherited ones "Inherited from <name>", teams with their member
count, "Limited access" on folders, the Links section, "Change owner…" for
the owner in My drive, Done); a shared drive's "Members…" is the same list
over the drive's access endpoints (`DriveAccessView` with two sets of
endpoints: the members' `DriveAccessDialog`, with "Leave drive" for whoever
has a grant of their own, and the superadmin's `SharedDriveAccessDialog`
in Settings → Drives, for any shared drive). A shared drive's own
settings (`/settings/drives/<id>`, `routes/_app/settings/drives.$driveId.tsx`,
for its managers, from the drive page's Settings) are `LevelSettingsCard`
at drive level: its public links, its trash and versions. Settings → Teams
(`routes/_app/settings/teams.index.tsx`: the user's teams and their
primary team, the superadmin's "All teams" and New team;
`teams.$teamId.tsx`: storage, members with Manager/Member, Add members,
the team's drives with the role members get, New drive, and "Team
defaults"). The settings levels are one pair of components
(`src/components/levels.tsx`, the logic in `src/lib/levels.ts`): for the
superadmin and team managers `LevelSettingsCard` (each setting with its
`LevelControl`: Users may change / Locked / A range / Not set, a range
with at least / at most), for users `LevelNote`, `lockedByOf` and
`MySettingField` ("Set by your administrator", "Set by <team>", a range's
bounds, "<team>'s default"); every page with levels uses them (trash and
versions, drop box with the user's AI models, notifications, AI, Drives'
Sharing card for links and who makes shared drives, encryption, Security's
two-factor). The settings endpoints are called without the SDK's zod
checks (`UNCHECKED`): the spec types a setting's `value` as an object
while it is any JSON value. Settings → Security ends with Account activity
(`account-activity.tsx`, `src/lib/activity.ts`). Menus, toolbars and drop
targets follow `my_role` (`src/lib/roles.ts`): a viewer gets no edit
actions, upload only adds files and sees its own; the trash follows
each item's `my_role`, the user's role on its drive (`trashActions`:
restoring an editor's, deleting for good the owner's or a shared drive's
managers').

The M7d pages (approved 2026-11-10; docs/M7D_DESIGN.md §7, §9): "Mirror…"
at a drive's top (`MirrorDialog` in `src/components/mirrors.tsx`: My drive
for its user, a shared drive for a viewer and above; shown only when the
user has storage of their own and the drive's `mirrors.allowed` is not
off; title 'Mirror "<drive>"', To (`listBackends`, the user's own), When,
Cancel / Start mirroring; a refusal in words by its `code`, `refusalText`).
Settings → Mirrors (`routes/_app/settings/mirrors.tsx`, in the settings
nav with storage of one's own or a mirror): a row per mirror (drive,
storage, schedule, last copy or "copying 1,204 of 3,880 files", the status
badge `MirrorStatusBadge`, a stopped one's reason) and its menu: Mirror
now, Change (the schedule), Eject…, Remove… (`RemoveMirrorDialog`, naming
the bucket and location: it deletes the copy). Mirrors are drives of `kind`
`mirror`: the sidebar's "Mirrors" group lists them, and a mirror's page is
the folder view at `/drives/<id>` titled "<name> · mirror", with Read-only,
Eject… (`EjectDialog`: "Make it a drive" in place, or "Make a copy"; a name
when its own is taken among the drives the user knows, `ejectName`, or
the server refuses it with `code` `name_taken`, `nameTaken`; refusals in
words by their `code`, `copy_running` too), its
trash, "<storage> · last copied <when>", and a banner when stopped (why
and when, `stopped_at`: "… the drive was deleted on 2 Nov."), waiting
for an unlock or failing (`statusBanner`). Its trash (`/trash?drive=<id>`,
an encrypted item's name opened with the drive's ids it kept,
`trashSealedAs`;
the select naming mirrors "<name> · mirror") and its files' versions
restore into the drive mirrored when `restores_into_drive` (else a file is
downloaded: `mirrorTrashUrl`, `restoreAction`). An encrypted folder of a
mirror (or of a drive ejected from one) opens with the drive's ids:
`sealedAs` gives a node its `source_id`, its parent's and its top's, for
the keys and names. A shared drive's Members… lists "Mirrored by <user>"
(`DriveAccess.mirrored_by`; the user's own with its storage and schedule).
`mirrors.allowed` is on a shared drive's settings page and
`mirrors.shared_drives` in Settings → Drives' Sharing card and the team
defaults, through `LevelSettingsCard` (the group "Mirrors"). The pure logic
is `src/lib/mirrors.ts`; the lists poll every 3 s while a mirror copies or
waits (`useMirrors`).

The M7e pages (approved 2026-11-10; docs/M7E_DESIGN.md §8, §10): Settings →
Federation (`routes/_app/settings/federation.tsx`, superadmin, in a
`SuperadminGate`: Off / Only these servers / Open as a radio group, each
list with "Edit list" (one host per line, `parseHosts`), this server's host
and key id, why it is off though chosen (`offReason`: no `PUBLIC_URL`), and
`LevelSettingsCard` of `federation.`). Settings → Contacts
(`routes/_app/settings/contacts.tsx`, every user): the contacts with
what their server is (`contactDetail`, `serverKind`: "Organizer", or the
`provider` its discovery names, e.g. Nextcloud) and Remove…, "Invite someone" (the invite's code and link, each with Copy), the
invites with "Copy link", Revoke… and who accepted them, "Accept an
invite"; it is OCM's `inviteAcceptDialog`: `?token=…&providerDomain=…`
opens the accept dialog with that invite (`inviteFromLink`), after a
sign-in too: every sign-in (password, two-factor, passkey, single
sign-on) goes on to `next`, any path of this origin (`allowedNext` in
`src/lib/account.ts`, the browser's URL parser deciding; the backend's
`allowed_next` checks it again). The Share
dialog (`share-dialog.tsx`) lists the user's contacts with their server and
offers an address typed (`withTypedAddress`, `matchesRecipient`): someone
on another server is looked up (`lookUpAddress`), offered only viewer,
editor, manager (`remoteRoles`; a whole drive upload only too) and only
what their server takes (`remoteProblem`: no whole drive without
`drives`, no encrypted folder without `encrypted_folders`), and added with
`NewGrant.address`; `Grant.remote` rows show the name, a server badge and
the state (`REMOTE_STATES`); an inherited one has the same badge. Shared
with me marks a share from another server with its sharer's address
(`remoteFrom`), Accept / Decline while pending, then "Add to my files…";
its columns leave Name the room (From and Role narrow, below the name
under `md` and `xl`, the actions as wide as their buttons). Whole drives from other servers
(`kind` `remote`) are the sidebar's "Other servers" group, with their
server and state (`remoteDriveLine`, `NavItem.detail`); at such a drive's
top `RemoteDriveBanner` (`remote-drive.tsx`) has Accept / Decline and its
error. A node from another server (`Node.remote`) shows its server (a globe
badge, a globe mark when placed in My drive) and never offers Share…,
versions, mirroring or a drop-box target (`FolderBrowser` leaves it out).
Every refusal is words by its `code` (`refusalSentence`, used by
`errorMessage`; `errorCode` in `src/lib/api.ts`), never by the message. The
`federation.` settings are the group "Other servers" of the levels pages
(team defaults, a shared drive's settings for `federation.allowed`). The
pure logic is `src/lib/federation.ts`.

Encrypted folders shared with another Organizer server (`organizer-e2e`,
docs/E2E_SPEC.md §12). The owner's side: a member with `remote` is the
share with them (`user_id` the share's id, the grant's), wrapped for like
any (`keysToPost`, new generations too); their grant's row says "Waiting
for them to set up encryption" once accepted while they have no public key
(`grantDetail`), and members no row shows are listed under it by their
address (`waitingElsewhere`). The member's side opens it as a mirror's
copy: keys unwrapped with the answer's `folder_id`, names opened with the
parent's `source_id` (`sealedAs`, also for a node with none of its own
yet), and new names, renames, moves and uploads sealed with the parent's
`source_id` (`sealingId`: asked again for a folder made a moment ago, and
refused while it has none). A 502 `remote_unreachable` for the keys is
"The owner's server is not reachable" (`keysError`), shown over the list.

## Dependencies and generated code

- Add a dependency or generate anything with the command its own docs
  recommend (`pnpm add`, `pnpm add -D`, `pnpm dlx shadcn@latest add`, ...),
  never by editing `package.json` or copying files in by hand.
- Prefer a well-known, maintained package over writing the same thing
  ourselves. Propose it first (name, version, why) and add it only once it
  is approved. The workspace refuses versions less than a day old
  (`minimumReleaseAge` in the root `pnpm-workspace.yaml`): take the newest
  that is older.

## Conventions

- **API:** only through the generated SDK (`organizer-sdk`, `packages/sdk`,
  not committed): its TanStack Query options and mutations
  (`organizer-sdk/react-query`) and types (`organizer-sdk/types`); errors are
  shown with `errorMessage` (`src/lib/api.ts`). The exceptions: the bytes of
  files (`src/lib/transfer.ts`: uploads, multipart or resumable, the drop
  box's multipart upload with its `note` and `read_contents` parts, and the
  download links), which the SDK's JSON validation cannot carry; the single
  sign-on button, a plain link (the browser must go to the provider); and
  the live events (`EventSource` on `/api/events` in `_app.tsx`: `changes`
  fetches everything again, `drop-box` the drop box's items); and the
  service worker (`public/sw.js`), which has no SDK and fetches
  `/api/notifications/news` itself.
- **Duplicated list:** the single sign-on error codes (`src/lib/sso.ts`) are
  the backend's `Failure` (`apps/server/src/oidc.rs`): change both.
- **Routing:** file-based TanStack Router, one file per route in
  `src/routes/`; `src/routeTree.gen.ts` is generated by the Vite plugin and
  committed — never edit it by hand.
- **UI: stock shadcn/ui.** Set up with `pnpm dlx shadcn@latest init` on
  **Base UI** with the **Nova** style (`components.json`: `base-nova`),
  lucide icons, the Geist font. Components and blocks go in with
  `pnpm dlx shadcn@latest add <name>` and are used as shadcn ships them:
  no restyling, no custom theme, no hand-written look-alikes. Imports use
  the `@/` alias (`vite.config.ts` and `tsconfig.json`), as shadcn writes
  them.
- **Theme:** shadcn's own tokens in `src/index.css` (written by `init`):
  colours only through them (`bg-background`, `text-foreground`,
  `bg-primary`, `text-muted-foreground`, `border-border`, ...), never
  hard-coded.
- **Tests:** Vitest, for the pure logic in `src/lib` (colocated
  `*.test.ts`, node environment). No component or snapshot tests; the
  flows are covered by the end-to-end smoke tests (`tooling/e2e`).
- TypeScript strict: unused imports and locals fail the check.
