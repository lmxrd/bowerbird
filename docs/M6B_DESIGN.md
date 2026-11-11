# M6b design proposal — sharing

Status: **accepted 2026-11-08**, **built** (§9); built after M6a, which it builds on. The second step of M6 ([M6_OUTLINE.md](M6_OUTLINE.md)).

## Decided (2026-11-08)

- **Fine-grained permissions**, as Nextcloud: read (always), create,
  change, delete, re-share; each on or off per share.
- **Where a share appears is the recipient's choice**: in a "Shared with
  me" section of the sidebar (not in their tree, not synced), or **placed
  as a folder anywhere in their tree**, where it is synced, in WebDAV and
  in the apps like any folder.
- **Public links**: view and download, edit, and file drop (upload only);
  each with an optional password and expiry.
- **User groups** (managed by the admin) to share with.
- **The drop box may place into shared folders** the user can create in.

## 1. Sharing with people and groups

- **Share…** in a file's or folder's menu: add users or groups, each with
  its permissions; change or remove later. The owner, and anyone with
  re-share, can share; a re-share never gives more than the sharer has.
- The recipient gets an in-app notice (the sidebar's "Shared with me"
  badge) and finds it there. **Add to my files…** places it as a folder in
  their tree (a folder picker); **Remove from my files** takes it back out
  (the share stays in "Shared with me"). Declining removes it for them.
- **Storage, versions and trash are the owner's**: a recipient's delete
  goes to the owner's trash, and their writes count against the owner's
  limit.
- A share to a group reaches its members as they are now and later: a new
  member sees it, a removed one loses it.

## 2. How a placed share works

A placed share is a **mount**: an entry in the recipient's tree that stands
for the owner's folder. Listing it lists the owner's folder; every
operation inside goes to the owner's tree, through its lock and journal,
after a permission check. So:

- The **change feed** of each recipient with it placed gets the changes
  under it (the owner's change rows are fanned out to the mounts), so the
  apps and the desktop sync follow it.
- **WebDAV** walks into it like a folder.
- A file shared on its own is placed as a file entry the same way.
- A mount cannot be moved into another share (no loops), and a share of a
  folder that holds a mount shares only the owner's own files.

This is the largest change of M6b: path resolution, the operations, the
change feed and WebDAV all learn mounts.

## 3. Public links

From the same Share… dialog, "Links": add a link of a kind (view, edit,
file drop), with an optional password (argon2 hashed) and expiry; copy it;
remove it. A link is a long random token (`/s/<token>`).

- **View**: a page with the file (preview and download) or the folder's
  listing (download of a file, or a folder as a zip).
- **Edit**: the same, plus upload, new folder, rename and delete in it.
- **File drop**: a page to upload files; nothing of the folder is shown.
- A password is asked once per browser session; wrong ones count against
  the sign-in limits. Links stop working when they expire, when the owner
  is disabled or removed, or when the file goes.
- Link pages are part of the web app, with no sign-in, and serve files
  with the same safe headers as downloads (`nosniff`, a sandbox CSP).

## 4. Groups

Settings → Users gains a **Groups** card: make a group, name it, add and
remove members. Shares to a group follow its members.

## 5. The drop box

Retrieval and proposals see the shared folders a user has **placed** and
can create in; placing there writes into the owner's tree (counted against
the owner). Shares only in "Shared with me" are not offered.

## 6. Tables and endpoints (outline)

```
groups (id, name)              group_members (group_id, user_id)
shares (id, node_id, owner_id, user_id | group_id, can_create, can_change,
        can_delete, can_share, created_by, created_at)
share_mounts (id, share_id, user_id, parent_id, name)   -- placed shares
links (id, node_id, owner_id, token_hash, kind, password_hash, expires_at,
       created_at)
```

`/files/{id}/shares` (list, add, change, remove), `/shared` (with me: list,
place, unplace, decline), `/files/{id}/links` and `/links/{id}`,
`/admin/groups`, and the public `/api/s/{token}/…` for link pages.

## 7. Pages (mockups)

1. The **Share dialog**: people and groups with their permissions, and
   links.
2. **Shared with me** (a sidebar section): what was shared, by whom, with
   "Add to my files…" and "Decline".
3. A **link page** (view, and file drop).
4. Settings → Users: the **Groups** card.

## 8. Decided (2026-11-08)

1. **The pages**: approved as mocked up.
2. **Order**: groups and shares with people (with mounts) first, then
   public links.

## 9. Built (2026-11-08)

As designed, in the decided order, with these choices made while building:

- **Tables** (migrations 0018, 0019): as §6, and `share_declines` (a
  decline is per user, so that a group's share goes for that member
  alone), `users.shared_seen_at` (the sidebar's badge), and `links.token`
  besides its hash (the dialog copies a link again, as pending invites
  do). A mount is keyed by **user and node**, not by share: a node shared
  with someone twice (directly and through a group) is placed once, with
  what both give.
- **Access** is computed when used (`shares::perms_of`): a re-share is cut
  to its maker's access every time, so taking the maker's away takes it
  from those they shared with, without rewriting rows. Shares and links go
  with their node, also when it goes to the trash (a restore does not
  bring them back), and with their owner. Shares of a disabled owner keep
  working (only links stop).
- **Mounts** have the owner's node id, so everything inside is addressed
  by the owner's ids; the recipient's entry (its parent and name) is
  theirs. A recipient's mounts never nest (a node above or below one
  already placed is refused), so a node has one place in their tree.
  Deleting the mount's entry (the API, WebDAV, the sync) takes it out of
  their files, never the owner's folder. Moves between a user's own files
  and a share, or between shares, are refused: a copy is the way.
- **Change feed**: the owner's rows are copied to the recipients' feeds at
  write time (`fan_out`), so a cursor and its 410 work as before; a node
  moving into a placed share appears with what is under it, moving out is
  a delete. Live events reach the recipients through `Files::notify`.
- **WebDAV** deletes a folder whole (as the API does) rather than file by
  file, so deleting a placed share never walks into the owner's files.
- **Folder zip**: the `zip` crate, already built in through xberg at the
  same version and features (no new crate), streamed from a thread;
  approved as a direct dependency 2026-11-08.
- **Not done**: search does not look into shares; the iOS, Android and
  desktop apps show placed shares as folders (through the listing and the
  feed) but have no sharing screens.
