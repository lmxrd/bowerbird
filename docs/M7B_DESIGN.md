# M7b design proposal — roles, teams and settings levels

Status: **accepted 2026-11-09** (§10). Step 2 of [M7_DESIGN.md](M7_DESIGN.md) (§9), on top
of M7a's drives ([M7A_DESIGN.md](M7A_DESIGN.md)). Storage backends are
step 3.

## 1. Roles

One model for every drive and every item in it, as Google Drive:

| Role | May |
| --- | --- |
| owner | everything, including delete for good and handing over |
| manager | who has access, and the item's or drive's settings; all an editor may |
| editor | read, write, rename, move, delete (to the trash) |
| viewer | read and download |
| upload only | add files, and see only their own uploads |

- **Who holds a role**: a user or a team (§3). A **grant** is (item or
  drive, user or team, role).
- **My drive**: its user is its owner; they give roles on folders and
  files in it (what M6b's shares did). **Shared drives** have no owner:
  their grants are on the drive, always with at least one manager.
- **Inheritance**: a grant applies to everything below. On a subfolder a
  manager can **widen** (more grants, or a higher role) or turn on
  **limited access**: from there down only the grants made on that folder
  or below count, and everyone else **does not see it at all** — listing,
  search, trash, versions, the change feed, sync, WebDAV, the drop box, and
  later mirrors. The owner and the drive's managers always keep access.
- **Upload only**: the listing shows only what that user added (nodes get
  `created_by`), and nothing else under it reaches them, in any of the
  places above.
- **What one has**: the highest role of every grant that reaches the item
  (theirs and their teams'). Every entry the API returns carries
  **`my_role`**, so all clients show only what the role allows.
- **Changing the owner** (owner, My drive only): another user becomes
  the item's owner and the previous owner becomes its manager. The item
  stays where it is.
- **Links** (M6b) stay as they are: view, edit or upload-only, made by an
  editor or above when the settings allow links (§4).

## 2. What changes for M6b and M7a

- **Shares become grants**: read only → viewer; with create, change or
  delete → editor; with re-share → manager (as decided in M7 §8.2).
  "Shared with me" and placing a share in My drive (mounts) work as now.
- **M7a's drive members** become grants on the drive: those who manage it →
  manager, the others → editor.
- **Groups become teams** (§3), with their members; until the superadmin
  names one, a team has no manager of its own.
- **Re-share** is the manager's "who has access", and a manager can give at
  most manager (never owner).

## 3. Teams

- **A team**: a name, members, its own **managers** (team roles use the same
  names: manager or member), a **storage limit** for all its drives
  together, and **setting defaults** (§4).
- **The superadmin** makes and removes teams and names their first
  managers. **Team managers** add and remove members and managers, make
  the team's shared drives, and set the team's defaults within what the
  superadmin left open.
- **A team's drives**: a shared drive can belong to a team. The team's
  managers are its managers; the team's members get the role the drive
  gives the team (editor by default). Others can be added to it as to any
  drive.
- **Who may make shared drives**: a setting (§4) — superadmin only, also
  team managers (default), or everyone.
- **A user in several teams**:
  - **drive settings follow the drive's team**, so a team's rules hold on
    its drives whoever works there;
  - **sign-in settings** (two-factor required): **the strictest** of their
    teams' wins, so joining a lax team never weakens a strict team's drives;
  - **their own My drive and preferences** (links, retention, AI, drop
    box, notifications): **the loosest** of their teams' locks and ranges
    wins;
  - **defaults** (nothing locked) come from their **primary team**: the
    first team they join, which they can change among their teams.

## 4. Settings levels

- **Levels**: superadmin → team → user (and the drive, for drive settings
  such as retention). Environment variables are above all of them and
  lock what they set.
- **At each level** a setting is:
  - **locked**: the value is fixed for everyone below (shown to them,
    disabled, with who set it);
  - **default**: the value is given, and those below may change it;
  - **not set**: those below choose; the built-in default applies until
    they do.
  Numbers can also be locked to a **range** (at least / at most): the
  privacy floor, such as "trash keeps at least 7 days".
- **Which settings get levels** (the rest stay the superadmin's alone,
  such as backups, single sign-on, sign-up and AI concurrency):

  | Setting | Levels |
  | --- | --- |
  | Drop box behaviour (pending mode, reading contents, folder freedom) | superadmin, team, user |
  | Retention (trash days, versions kept) | superadmin, team, user, drive |
  | Notifications (review, shared, backup) | superadmin, team, user |
  | AI: the model for each kind, from the superadmin's models | superadmin, team, user |
  | AI: users may add their own models (off by default) | superadmin |
  | Public links: allowed, password required, longest lifetime | superadmin, team, drive |
  | Encryption: users may make encrypted folders | superadmin, team |
  | Sign-in: two-factor required | superadmin, team |
  | Who may make shared drives | superadmin |

- **Storage**: one table, `settings (level, level_id, key, value, mode,
  min, max)`, read through one function that walks the levels; the
  existing tables (`drop_box_settings`, `retention_defaults`,
  `retention`, `notification_settings`, the AI defaults) move into it.
- **The superadmin's and team managers' pages** show, beside each setting,
  a small control: **Users may change** / **Locked** / **Not set**.
  Users see locked settings disabled, with "Set by your administrator" or
  "Set by <team>".

## 5. The superadmin's actions, seen by users

- **A log** of what the superadmin (or the server's command line) did to
  an account: made or removed it, disabled it, changed its role, storage
  limit or teams, reset its two-factor, reset its password, changed a
  drive's members it is in.
- **Each user sees theirs** in Settings → Security → **Account activity**,
  with when, who and what. Lock changes that reach everyone are not in
  it (they show on the settings themselves).
- **The superadmin still sees only what they own or what is shared with
  them** (M7 §2): no setting or role here lets them into a drive they are
  not a member of.

## 6. The API

- `GET/POST/PATCH/DELETE /api/files/{id}/access`: grants (user or team,
  role) and **limited access**; in place of `/api/files/{id}/shares`.
  `/api/drives/{id}/access` for a drive's grants.
- `/api/teams` (mine; the superadmin's under `/api/superadmin/teams`),
  `/api/teams/{id}/members`, `/api/teams/{id}/settings`.
- `GET /api/settings` (mine, each with its value, where it comes from and
  whether I may change it); `PUT /api/settings/{key}` (mine);
  `/api/superadmin/settings` and `/api/teams/{id}/settings` with the modes.
- `GET /api/me/activity` (the log of §5).
- `my_role` on every node; `PATCH /api/files/{id}/owner` to change the owner.

## 7. The clients

- **Web**: the **Share** dialog with roles per person and team, a
  "Limited access" switch on folders, and handing over; **Settings →
  Teams** (the superadmin's list; a team's page with members, managers,
  drives, limit and defaults); the **level control** on every setting with
  levels; **Account activity**; menus that offer only what `my_role`
  allows.
- **Desktop, iOS and Android**: follow `my_role` (no edit actions for a
  viewer; an upload-only folder shows only one's own files and allows
  adding); sync treats a viewer's folders as read-only (local changes are
  not sent and are reported). No sharing screens on them yet.

## 8. Migration

Migrations from 0026: `grants` from `shares` and `drive_members` (§2),
`teams` and `team_members` from `groups` and `group_members`,
`nodes.created_by` (the owner for what exists), `limited` on nodes,
`settings` from the existing settings tables (each user's values as user
level, the instance defaults as superadmin level with mode default), the
activity log. Old `/api/files/{id}/shares` and `/api/superadmin/groups`
answer for one release.

## 9. Pages (mockups)

1. The Share dialog with roles, teams and limited access.
2. Settings → Teams, and a team's page.
3. A settings page with the level control (as the superadmin, and as a
   user seeing a lock).
4. Account activity.

## 10. Decided (2026-11-09)

1. **The design** and **the pages** as proposed and mocked up, with the
   changes below.
2. **Several teams**: as §3 (drive settings by the drive's team, sign-in
   strictest, own settings loosest, defaults from the primary team).
3. **Shared drives** are made by the superadmin and team managers by
   default.
4. **Changing the owner**: the previous owner becomes manager; the item
   does not move (in place of the proposed move into the new owner's My
   drive with editor).

## 11. Built (backend)

Migrations 0026 (roles and teams: `grants`, `grant_declines`, `teams`,
`team_members`, `users.primary_team_id`, `drives.team_id`,
`nodes.created_by` and `nodes.limited`, `trash_marks`, the `drive_users`
view; `shares`, `drive_members` and the groups dropped after their rows
moved) and 0027 (`settings`, from `drop_box_settings`, `retention`,
`retention_defaults` and the notification switches; `activity`).
`files/roles.rs` (what a user has), `files/grants.rs` (changing it),
`settings.rs` (the resolver), `activity.rs`; the routes `access.rs`,
`teams.rs`, `settings.rs`, `me.rs`. Choices made while building:

- **One table of grants**, for items (`node_id`) and whole shared drives
  (`node_id` NULL), to users or teams. The owner of a handed-over item is
  an `owner` grant on it (the nearest one above counts); without one, My
  drive's user owns everything in it. Teams kept the groups' ids, so a
  share to a group is the team's grant.
- **Roles are ordered** upload only < viewer < editor < manager < owner, and
  what one has is the highest that reaches the item. **Keepers**, whom
  limited access never hides anything from: the owner, a shared drive's
  managers (and its team's managers), and My drive's user on an item they
  handed over (they manage it).
- **Upload only** adds files and folders where it was given, and sees,
  reads and downloads only that folder and what it added; it does not
  rename or delete what it added, nor see who has access. It is not a
  place for the drop box (whose proposals need to see the folders).
- **Links** need an editor or above (`Need::Link`). The link settings are
  checked when a link is made or changed, and when one is used: links
  turned off where they are stop working.
- **M6b's re-share rule stays**: a grant on an item given by someone who
  is not a keeper there gives no more than its giver still has (min of the
  two roles), so taking theirs takes it from those they gave it to. Grants
  on a drive are never cut (its managers or the superadmin give them). A
  giver removed from the server leaves the grant standing (no giver).
- **Deleting for good** (a trash item, the whole trash, a version) is the
  owner's, and on a shared drive (no owner) its managers'; M7a let every
  member do it. Restoring takes an editor. The trash a user sees leaves
  out what was limited and what was in a folder hidden from them (upload
  only: what they did not add). Grants end with their item in the trash,
  as shares did; what was limited, and who added what, is kept by path
  (`trash_marks`) and given back on a restore, so a restore never opens a
  limited folder.
- **The change feed**: when a drive hides nothing (no limited folder, no
  upload-only user), a row goes to every user of the drive as before;
  otherwise per user who sees the node. A move into or out of what a user
  sees is the subtree appearing (as `create`, parents first, only what
  they see) or a `delete`; so is a change of access (a grant, limited
  access) for a drive's users. A drive role that becomes or stops being
  upload only sends the drive's root as deleted and created again, so the
  apps list it anew.
- **Search** reads four times the page and leaves out what the user does
  not see: a page may be shorter than `limit`. Hits carry `my_role`.
- **WebDAV** shows My drive and placed shares (as before); inside a placed
  share what is hidden is left out of listings and any path into it is a
  404.
- **A shared drive keeps a manager**: a change that would leave one that
  had a manager without one is refused (409). A drive the server made
  without one (the sync engine's tests) may still get members first.
- **Teams**: removing one is refused while one of its drives would keep no
  manager; its members leave it first (each noted in their activity). A
  team drive gets the team's grant (`editor`, or what its maker chose of
  editor, viewer, upload only) and is managed by the team's managers.
  `POST /teams/{id}/drives` makes one (team managers, unless
  `drives.create` is `superadmin`); `POST /drives` makes a drive without a
  team when `drives.create` is `everyone`; the superadmin's
  `POST /superadmin/drives` takes a `team_id`, and
  `PATCH /superadmin/drives/{id}` moves a drive to a team or out of one.
- **Settings**: a third mode, `range`, holds only `min`/`max` (a default may
  carry a range too); a number out of the range of the levels above is held
  to it when resolved, and refused when written. The keys:
  `drop_box.pending_mode`, `.accept_after_hours`, `.confidence_threshold`,
  `.read_contents`, `.folder_freedom` (superadmin, team, user);
  `drop_box.allowed_top_folders`, `.folders`, `.links` (user only, moved
  with the rest of the table); `retention.trash_days`, `.versions_hours`,
  `.versions_days`, `.versions_months` (superadmin, team, user, drive);
  `notifications.review`, `.shared`, `.backup`; `ai.model.proposals`,
  `.images`, `.scans`, `.audio` (a model's id, null: the AI page's
  choice); `ai.own_models` (stored only: users' own models are for later);
  `links.allowed`, `.password_required`, `.max_days` (0: no limit;
  superadmin, team, drive); `encryption.allowed`;
  `sign_in.two_factor_required`; `drives.create` (`superadmin`,
  `team_managers` by default, `everyone`).
- **The AI page's choice of model per work stays in `ai_settings`** (its
  rows are tied to the registered models and `AI_MODEL_*`): it is the
  built-in value of `ai.model.*`, which the superadmin may also lock or
  replace there, and teams and users choose among the registered models
  that can do the work. The embeddings model stays the instance's (the
  vector store is reset when it changes).
- **My drive's drive level**: for a setting with a user level (the trash
  and versions) a user's own value is their My drive's; for one without
  (links) My drive's drive row, which its user sets
  (`/drives/{id}/settings`).
- **Several teams**: strictest = locked when any team locks it (`true`
  first); loosest = locked only when every team locks it (the primary's
  value), a range only as wide as the widest (no bound where a team has
  none); values otherwise from the primary team. A setting's `source`
  names the team it comes from.
- **The old settings endpoints** (`/drop-box/settings`, `/retention`,
  `/superadmin/retention`, `/notifications/settings`) read and write the
  levels: a user's change is their level's (403 when locked above);
  `/auth/session`'s `totp_required` is the signed-in user's.
- **The activity** notes: made through a superadmin's invite (password or
  single sign-on), disabled, enabled, superadmin granted or revoked
  (endpoint or command line), storage limit, files folder, team added,
  removed or role changed, two-factor reset (command line), access to a
  shared drive (the superadmin's drive endpoints). Removing an account
  removes its log; there is no superadmin password reset yet, so
  `password_reset` is reserved. What a team's managers do is in it too
  (below).
- **Not done here**: grants are not shown in the sync engine's local
  state (the clients follow `my_role`, step 7); the load test was not run
  (the change feed and the listings ask more of the database where a
  drive hides something: worth a run).

### The gaps closed (2026-11-09)

- **The models one may choose**: `GET /ai/models` (any user,
  `listSelectableAiModels`): for each work with a user level (proposals,
  images, scans, audio) the models that can do it, by name and id only,
  never an endpoint or a key. The web's model selects (the drop box's
  models, the level pages) offer them to everyone, not only the
  superadmin; a chosen model no longer listed shows by its id.
- **A drive's own settings**: Settings → Drives → a drive
  (`/settings/drives/<id>`), for a shared drive's managers, from the drive
  page's Settings button: `LevelSettingsCard` at drive level (its public
  links, its trash and versions), within what its team and the superadmin
  left open; others see that only its managers change them.
- **The superadmin's drives**: Settings → Drives' Members… is the Share
  dialog's list over `/superadmin/drives/{id}/access` (GET, POST, PATCH,
  DELETE: any shared drive, the superadmin a member of it or not; `my_role`
  is `manager` there, what they may do to its access; at most manager; a
  user's grant, change or removal noted in their activity, a team's not),
  and New shared drive has a Team field. M7a's member endpoints
  (`/drives/{id}/members`, `/superadmin/drives/{id}/members`) and
  `SharedDrive.member_list` are gone (nothing used them). Leaving a drive
  is giving up one's own grant (`DELETE /drives/{id}/access/{grant}`, which
  now takes the grant's own user too, not only a manager: an editor or a
  viewer could not leave before).
- **Trash items carry `my_role`**: the user's role on the item's drive
  (also on `DELETE /files/{id}`'s answer; null for a placed share taken
  out of one's files, which goes to no trash). The web's trash offers
  Restore and Delete forever per item from it (`trashActions`).
- **What a team's managers do** (adding someone to the team, changing
  their team role, taking them out) is in that user's activity, with the
  actor `team_manager` and the manager's email (migration 0028 rebuilds
  `activity` for the new actor). What one does oneself (leaving, changing
  one's own role) is not noted. Account activity names them "<email>,
  manager of <team>".
- **Upload only on iOS** still offers New folder (the server allows
  folders): unchanged.
- **Checked**: `mise run check --force` and `mise run --force test`
  pass (the backend's tests: `users_list_the_ai_models_they_may_choose`,
  `trash_items_carry_the_drive_role`,
  `team_managers_actions_are_in_the_account_activity`,
  `the_superadmin_gives_access_to_shared_drives`,
  `a_shared_drives_managers_set_its_settings`; Vitest for `modelChoicesFor`,
  `modelItems`, `trashActions`, `activityActor`, `teamOptions`), and so do
  `check:ios` and `check:android` on the new spec. Tried in a browser
  against a local debug server: the superadmin made a team and a shared
  drive with it, and gave a user editor access from Settings → Drives'
  Members… without being a member; the team's manager added that user to
  the team and set the drive's trash days on its Settings page; the user
  saw both in Account activity (the manager as "<email>, manager of
  <team>"), only Restore in the drive's trash (an editor), and the
  environment's model in the drop box's model select.
- **Still open**: the trash page says when items go by the user's own
  trash days, not a shared drive's (the drive's own setting is applied by
  the server); a team grant on a drive given by the superadmin is not
  noted in the team members' activity.
