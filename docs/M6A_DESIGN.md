# M6a design proposal — more than one user

Status: **accepted 2026-11-08** (§5), **built** (§6). The first
step of M6 ([M6_OUTLINE.md](M6_OUTLINE.md)). Today the first admin is the
only user. Everything is already keyed by user (files, drop box, settings,
versions, trash), so this adds the ways in and the admin's management.

## 1. Ways in (decided)

- **Invite links.** The admin makes a link (optionally for one email, and
  optionally as an admin) and sends it themselves. It is good for 7 days
  and used once. The link's page asks for the email (filled in when the
  invite names one) and a password, or offers single sign-on; then the
  account exists and is signed in.
- **Single sign-on creates users** when the admin allows it: someone the
  provider signs in, with a verified email no account has, gets an account
  on that first sign-in. Off: they are refused, as today.
- **Open sign-up** when the admin allows it: "Create an account" on the
  login page, email and password.
- Settings → Users has both switches; `SIGNUP_OPEN` and
  `OIDC_CREATE_USERS` lock them.
- With `OIDC_ONLY`, passwords are off for everyone: an invite then offers
  single sign-on only, and open sign-up is single sign-on's "create users".

## 2. The admin's list

Settings → Users: each user's email, role, storage used (and limit), when
last active, and their state. A user's menu:

- **Make admin / Remove admin.**
- **Storage limit**: none, or so many GB. Writes through Organizer that
  would go past it are refused (507 "Insufficient Storage"): uploads, the
  drop box, WebDAV, versions count toward it, the trash too. Changes made
  directly in a files folder are not stopped, only counted.
- **Files folder**: where their tree is (default
  `<DATA_DIR>/users/<id>/files`; an existing folder, as the admin's own).
- **Disable / Enable**: a disabled user cannot sign in, and their sessions,
  API keys and device tokens stop working; nothing is deleted.
- **Remove**: the account, its sessions and keys, and Organizer's own data
  for it (drop box, trash, versions); its files folder is deleted only when
  it is the default one inside the data dir, never an existing folder.
  Asked twice (type the email).

There is always at least one admin who is not disabled: the last one cannot
be demoted, disabled or removed.

## 3. Tables and endpoints

```sql
ALTER TABLE users ADD COLUMN disabled_at TEXT;
ALTER TABLE users ADD COLUMN storage_limit INTEGER;     -- bytes; NULL: none
ALTER TABLE users ADD COLUMN last_seen_at TEXT;
CREATE TABLE invites (
  id BLOB PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE,
  email TEXT, is_admin INTEGER NOT NULL DEFAULT 0,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL, expires_at TEXT NOT NULL, used_at TEXT);
CREATE TABLE signup_settings (id INTEGER PRIMARY KEY CHECK (id = 1),
  open_signup INTEGER NOT NULL DEFAULT 0, oidc_create_users INTEGER NOT NULL DEFAULT 0);
```

| Method and path | What |
| --- | --- |
| `GET /admin/users` · `PATCH /admin/users/{id}` · `DELETE /admin/users/{id}` | The list; role, limit, files folder, disabled; remove |
| `GET /admin/invites` · `POST /admin/invites` · `DELETE /admin/invites/{id}` | Pending invites; make one (the link once); revoke |
| `GET`/`PUT /admin/signup` | The two switches, with their locks |
| `GET /auth/invite/{token}` · `POST /auth/invite/{token}` | An invite's state (its email); accept it |
| `POST /auth/signup` | Open sign-up |

`/auth/session` says whether sign-up is open, for the login page. Invites
and sign-up count against the sign-in limits (per address and email).

## 4. Pages (mockups first)

1. **Settings → Users** (admin): the sign-up switches, the users table with
   its menus, "Invite someone" (a dialog: email, admin, then the link with
   Copy), pending invites with Revoke.
2. **The invite page** (`/invite/<token>`): the setup page's form, with
   "Joining <server> as <email>".
3. **Login** gains "Create an account" when sign-up is open, and
   **`/signup`** is the same form.
4. The user's own storage is shown in Settings → Account (used of limit).

## 5. Decisions (2026-11-08)

1. **The pages**: approved as mocked up.
2. **Removing a user** deletes Organizer's data for them and their files
   folder only when it is the default one in the data dir; an existing
   folder is never deleted.

## 6. Built (2026-11-08)

As designed, with these choices made while building:

- `FILES_DIR` is the first admin's folder only (`users.owns_files_dir`, set
  at setup and, for an existing instance, on its oldest user), so removing
  that admin never hands it to someone else. A files folder must be apart
  from every other user's.
- An invite keeps its token (as well as its hash) while it is pending, so
  that "Copy link" works on the pending list; it is cleared when used.
- An invite accepted through single sign-on starts at
  `/api/auth/oidc/login?invite=<token>`; the token rides in the signed
  login cookie. A provider email that already has an account is linked as
  before and leaves the invite unused.
- Disabling hides the user from the session loader and token lookups, so
  their sessions and tokens work again once enabled (nothing is deleted).
  A disabled user is told so only after the right password.
- The storage limit is checked before a write (not under one lock across
  writes): two writes at once may together pass it a little.
- `last_seen_at` is written on sign-in and on a request, at most once a
  minute. The admin's menu does not offer to disable or remove themselves.
- With `TOTP_REQUIRED`, a new account sets up two-factor authentication
  first, as at a sign-in.
