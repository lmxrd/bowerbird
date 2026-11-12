# Next steps

What is left after M7 (as of 2026-11-10, branch `feat/m0-foundations`).
Tick an item off by removing it; add new ones under the right heading.

## M7e (federation)

- [ ] **Run the Nextcloud check**: `mise run interop:nextcloud`
      (`scripts/interop-nextcloud.sh`, image `nextcloud:35.0.2-apache`).
      It is written but has not run, because it pulls an image.
  - [ ] It should also settle one doubt: while the other server was down,
        ours stored an empty root ETag. So the ETag fallback, used for
        servers without sync-collection such as Nextcloud, may not read
        the root's ETag. Every follow of such a share would then be a full
        rescan.
- [ ] **Move inside an encrypted folder on iOS and Android.** The apps
      say "Move it on the web".
- [ ] **Sharing screens in the apps.** Remote grants (`Grant.remote`) and
      the owner's view of members on other servers waiting for a key are
      on the web only. iOS and Android have no sharing or access-list
      screens.
- [ ] **Shared with me on the desktop.** It has no such view, so items
      from another server show only as drives there.
- [ ] **Streamed uploads into a remote share.** An upload is read whole
      into memory and sent in one PUT, because opendal's WebDAV writer
      (0.58) cannot stream. Fixing it means a PUT of our own next to
      opendal (see the comment in `Storage::upload`).
- [ ] **Try in a browser:**
  - [ ] an encrypted folder shared between two servers (owner wraps,
        member opens and writes);
  - [ ] a whole remote drive's error banner (`RemoteDriveBanner`).
- [ ] **Flaky test:** `link_passwords_are_limited_as_sign_ins`
      (`apps/server/tests/api.rs`) sometimes gets 204 instead of 429
      under load. It is time-based; make it deterministic.

## From earlier milestones

- [ ] **Real S3.** Storage backends have only been tried against
      in-memory stand-ins and a Python fake, never a real provider.
- [ ] **The apps by hand.** Try the desktop app, the iOS app (and the
      macOS File Provider) and the Android app on real devices or
      simulators.
- [ ] **CI.** Nothing is pushed yet, so no workflow has run on this work.
- [ ] **Load test.** Worth a run after M7c's storage layer and M7e's
      change log (`mise run loadtest`, manual).
- [ ] **Approve the drive settings page.** `/settings/drives/<id>` was
      built without an approved mockup.
- [ ] **Trash's "Goes in N days"** uses the user's trash days, not the
      drive's.
- [ ] **Commit titles to fix when squashing.** Some commits are typed
      wrong, for example 9c70113 (`test`, but it carries fixes): give
      the pull request a `feat` title.
- [ ] **Pull request.** None opened yet for `feat/m0-foundations`.

## Working rules for parallel agents

- **One Rust builder at a time.** All builds share `~/.cargo-target`.
  Worktrees of this repository get the same cargo unit hashes there, so
  a build can count as fresh from another worktree's sources. Other
  agents may work in parallel only on web, iOS or Android, from a spec
  copied in once.
- **Touch first.** The agent that builds Rust touches its changed `.rs`
  files before its final build.
