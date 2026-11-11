# M6 outline — beyond the MVP

Status: **accepted 2026-11-08** (the decisions below). Milestone M6 of [ARCHITECTURE.md](ARCHITECTURE.md)
(§7) names five things of very different size. This outline proposes an
order and the decisions each needs; each step then gets its own short
design (with mockups for its pages) before it is built.

## The steps, in the proposed order

1. **M6a — more than one user.** Today there is one admin and nobody else
   (the data model already keys everything by user, as ARCHITECTURE.md §2
   planned). Adds: inviting users, their own files folder each, the admin's
   list (disable, make admin, remove), per-user limits on storage if wanted.
   Everything else (drop box, AI, versions, backups) is per user already.
2. **M6b — sharing.** Between users: a folder shared with another user,
   read-only or read-write, showing in their tree under "Shared with me".
   And links: a read-only link to a file or folder for anyone, with an
   optional expiry and password. Needs M6a for the first.
3. **M6c — reading more.** OCR of scans and photos of text, a vision model
   describing images, and audio transcribed, so the drop box can place them
   and search can find them. Through the AI connection the admin already
   set up (a vision or speech model of the same provider), and/or xberg's
   OCR. The drop box's guard and "read contents" settings apply as for text.
4. **M6d — push notifications.** "3 items to review" on a phone that is not
   open: UnifiedPush (self-hosted, no Google or Apple account) for Android,
   APNs for iOS (needs an Apple team, like M5b), FCM optional.
5. **M6e — end-to-end-encrypted folders.** Folders whose contents the
   server cannot read. They cannot be read by the AI, searched or
   previewed on the server, and need client-side encryption in every app
   (web, iOS, Android, desktop). The largest step and the one that cuts
   against the drop box; proposed last, and only if still wanted then.

## The decisions (questions)

1. The order above.
2. **How users join** (M6a): invite links the admin makes and sends
   themselves (no email sending in Organizer), plus single sign-on users
   created on first sign-in when the admin allows it. Or open sign-up.
3. **Sharing** (M6b): both user-to-user folders and public links, user
   folders first.
4. **Reading more** (M6c): through the AI connection's vision and speech
   models (no new native libraries; the settings say what is sent), with
   xberg's OCR only if it builds into the static binary.

## Decided (2026-11-08)

1. **The order** as above.
2. **Joining**: invite links from the admin; single sign-on users created
   on their first sign-in when the admin allows it; and open sign-up when
   the admin allows it. Both switches are settings with a variable that
   locks each (as the `AI_*` and `OIDC_*` ones do).
3. **Sharing**: what Google Drive and Nextcloud offer, roughly; the details
   are talked through before M6b is designed.
4. **Reading more**: the admin chooses in the settings how OCR, images and
   audio are read (the AI connection's models, a local engine, or off).

## Progress (2026-11-08)

M6a (users), M6b (sharing), M6c (models and reading) and M6d (push) are
built. **M6e (end-to-end-encrypted folders): not now**, by decision; it
can be designed later.
