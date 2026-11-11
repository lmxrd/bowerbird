# Organizer — notes for coding agents

Organizer is a light, self-hosted file server with an AI drop box: a Rust
backend (axum, SQLite through sqlx) with a React frontend embedded in the
one binary. It is at milestone M0 (foundations); the plan, the decisions
and the open questions are in `docs/ARCHITECTURE.md`, which wins over
anything you would assume. README.md is the user documentation and
CONTRIBUTING.md the contributor guide. It is built the same way as its
sibling projects Potoo and Woodmouse: when in doubt, do what they do.

## Commands

Everything goes through [mise](https://mise.jdx.dev) (`mise install` once;
it provides Rust, Node, pnpm and the other tools at the pinned versions).

| Command | Use |
| --- | --- |
| `mise run check` | rustfmt, clippy (`-D warnings`), oxfmt, oxlint, tsc. **Must pass.** |
| `mise run test` | The unit and integration tests: backend (cargo, axum-test) and frontend (Vitest). **Must pass.** |
| `mise run test:e2e` | The end-to-end smoke tests: builds the image, then Playwright drives a browser through it (`packages/e2e`, `scripts/e2e.sh`) |
| `mise run fix` | Applies the format and lint fixes |
| `mise run dev` | Backend on :8000 plus the Vite dev server |
| `mise run eval` | The drop box's eval set (`packages/backend/tests/eval`, synthetic) against the model of the `AI_*` variables: manual, never in CI |
| `mise run interop:nextcloud` | Federation with a Nextcloud container (podman, `scripts/interop-nextcloud.sh`): shares both ways over OCM. Manual, never in CI: it pulls an image; run it only when asked |
| `mise run gen:spec` | Writes the OpenAPI spec to `packages/backend/target/specs/openapi.json` |
| `mise run gen:sdk` | Generates the TypeScript SDK (`packages/sdk/src`) from the spec |
| `mise run build` / `build:linux` / `build:docker` | Release binary (`dist/organizer`) / static Linux binary (`dist/organizer-linux-<arch>`, zig cross-compiles) / image (podman) that copies it |
| `mise run audit` / `audit:image` | Known vulnerabilities in the lockfiles (osv-scanner) / in an image's Alpine packages (Trivy; `IMAGE=`, by default the published one) |

Run `mise run check` and `mise run test` before you call a change done.
Tasks with `sources`/`outputs` are skipped when unchanged (`--force` runs
them anyway). Use `DATA_DIR=/some/tmp/dir` when you run the server and must
not touch `data/`.

## Structure

- `packages/backend`: the Rust server (its rules: `packages/backend/AGENTS.md`).
- `packages/frontend`: the React app embedded in it (`packages/frontend/AGENTS.md`).
- `packages/sdk`: the API client, generated from the backend's OpenAPI spec
  (`mise run gen:sdk`), never committed.
- `packages/e2e`: the end-to-end smoke tests (Playwright).
- `packages/ios`: the iOS app (below).
- `packages/android`: the Android app (below).
- `packages/e2e-crypto`: end-to-end-encrypted folders' reference
  implementation (docs/E2E_SPEC.md), the test vectors every client checks
  (`vectors.json`, `mise run gen:vectors`) and `organizer-decrypt`
  (`mise run build:decrypt`); `check:crypto` and `test:crypto` are part of
  `check` and `test`.
- `packages/sync`, `packages/cloudfiles` and `packages/desktop`: the
  desktop app's sync engine, its Windows Cloud Files crate, and the app
  itself with its macOS File Provider extension (below).
- One pnpm workspace and lockfile at the root, and one Cargo workspace and
  lockfile at the root (`packages/backend`, `packages/e2e-crypto`, `packages/sync`,
  `packages/cloudfiles`, `packages/desktop/src-tauri`, one target
  directory); `scripts/` for the
  Linux build, the end-to-end run and the desktop bundles; `.github/` for CI, releases, Dependabot and
  the daily security scans.

## iOS app (`packages/ios`)

SwiftUI, iOS 18 or later (docs/M4_DESIGN.md). Xcode on a Mac; not part of
`check` and `test`.

| Command | Use |
| --- | --- |
| `mise run gen:ios` | `Organizer.xcodeproj` from `project.yml` (XcodeGen), after `gen:spec` |
| `mise run build:ios` | The app and its share extension for the simulator, unsigned (`scripts/build.sh`) |
| `mise run test:ios` | The tests on an iPhone simulator (`scripts/test.sh`; `SIMULATOR=<udid>` picks one) |
| `mise run check:ios` / `fix:ios` | `swift format lint --strict` / `swift format` on `Sources` and `Tests` |

- **Generated, not committed:** `Organizer.xcodeproj` and the Info.plist
  and entitlements files XcodeGen writes from `project.yml`; the client,
  made at build time by swift-openapi-generator's build plugin from
  `Sources/OrganizerKit/OrganizerKit/openapi.json`, which `scripts/spec.sh`
  copies from the backend's spec (making a nullable reference optional:
  the generator skips `null` schemas).
- **Layout:** `Sources/Organizer` (the app), `Sources/ShareExtension`,
  `Sources/OrganizerKit` (a local Swift package both use: the client, the
  session in the Keychain, the upload queue, the view models, and the
  views the extension shares), `Tests/OrganizerTests` (Swift Testing,
  hosted by the app: the upload client against a stub server behind a
  URLProtocol, the device login, the Keychain, the view models against a
  stub transport, the File Provider logic against a stub tree).
- **OrganizerKit on macOS too** (`.macOS(.v14)`): the desktop app's File
  Provider extension (`packages/desktop/macos`) links it statically (the
  `OrganizerKitStatic` product; the iOS targets link the dynamic one).
  Its views are iOS only (`#if os(iOS)`). `OrganizerKit/FileProvider` is
  the extension's logic, kept here so that it is tested here (and can give
  the iOS app its place in the Files app later): `ProviderItem` (a node as
  an `NSFileProviderItem`, the domain's folder as the root container, the
  server's version as the item's), `ProviderScope` (which changes of the
  feed are in the domain), `ProviderClient` (listings, the change feed
  page by page, downloads, and creates, modifies and deletes at the item's
  version, a moved-on version becoming a conflict copy named as the sync
  engine names them), `DesktopSession` (the token the extension reads).
  A domain's folder is a drive's top or a folder in one (docs/M7A_DESIGN.md):
  the scope learns its drive (and the drive's top) once and leaves the
  feed's changes of other drives out; the delete of the domain's folder
  or of its drive's top (the user left the drive, or it was deleted) is
  checked, then the working set fails with `noSuchItem` and nothing is
  told as gone (the app removes the domain, which keeps unsent changes
  aside). `.organizer` is never listed, made or renamed to at a drive's
  top.
- **Drives** (`Models/DrivesModel.swift`): the Files tab starts with the
  drives (`GET /api/drives`: My drive, each shared drive with "N members ·
  size", one whose `root_id` is null shown unavailable) and Shared with
  me (`GET /api/shared`, a folder opening as the top of its own tree, a
  file in Quick Look). A drive opens at its `root_id` in the usual folder
  view, titled with its name; a `FolderChoice` carries its `DrivePlace`,
  so Move's picker stays in the item's drive (the server refuses moves
  between drives). The share sheet's and the drop box's folder picker
  starts with the available drives (members may write in a shared drive);
  uploads into one go the usual way, encrypted folders in one too. The app
  has no search or trash of its own (both on the web).
- **Roles** (docs/M7B_DESIGN.md §7, `Models/Access.swift`): the app
  follows `my_role` of every node and drive, as the server's
  `Role::allows`: `ItemActions` of a role (open; add, i.e. upload and
  new folder, for upload only and up but not a viewer; rename, move and
  delete for an editor and up). A folder knows its role from where it was
  opened (`FolderChoice.role`: the node's, the drive's, a shared item's,
  `GET /files/root` for My drive's top); no role known leaves it all to
  the server. A viewer sees no "+" and no row actions; an upload-only
  folder says one sees only what one added (the server lists nothing
  else). The pickers (share sheet, drop box's "Elsewhere", Move) open
  any folder but choose only one that can add. The File Provider's
  `ProviderItem.capabilities` come from the same mapping (a viewer reads,
  upload only reads and adds to a folder, writing a file is an editor's),
  and a 403 is `fileWriteNoPermission`. No sharing screens on iOS.
- **Storage** (docs/M7C_DESIGN.md; no backend screens on iOS): each
  `Drive`'s `storage` (`Models/DrivesModel.swift`: ready, unavailable with
  the server's reason, locked when its `unavailable` is `locked`, moving). The
  drives list shows an unavailable drive with its reason (a lock for a
  locked one) and no actions (`DrivePlace(drive)` is nil), a moving one as
  "Moving to <backend>…": `DrivePlace.readOnly`, so its folders offer only
  open and the pickers leave it out. A drive on someone's own storage
  (`backend.owner`) shows its warning on its folders. `APIError` keeps the
  body's `code`: a 503 `storage_unavailable` reads "This drive is not
  available now: <reason>."; a 409 `read_only` (the drive is being moved) is `isReadOnly`,
  never `isConflict`. The File
  Provider maps both to `serverUnreachable` (the system retries), and the
  upload queue retries them.
- **Mirrors** (docs/M7D_DESIGN.md §5; no mirror screens on iOS): a
  `Drive` of `kind` `mirror` (`isMirror`, `mirror_of`) is in the drives
  list's own "Mirrors" section after the shared drives
  (`DrivesModel.mirrors`; `ownAndShared` the first section), titled
  "<drive> · mirror" (`title`) with its state (`mirrorLine`, by the
  status's and `stopped`'s codes: up to date, copying, waiting for unlock,
  failing, stopped and why). Its `my_role` is viewer; `DrivePlace.isMirror`
  makes it `readOnly` too (open only), and the pickers leave it out
  (`available`). An encrypted folder in a mirror has no keys of its own:
  `FolderChoice.sourceID` / `FolderModel.sourceID` (the node's
  `source_id`) is what its keys are asked by and its names opened with.
  The File Provider reports a mirror's items read-only by their role.
- **Remote drives** (docs/M7E_DESIGN.md §5, §6, §10; no sharing or
  contact screens on iOS: sharing with `user@host` is on the web): their
  bytes go through the user's own server, so a remote folder is a folder
  and a drive of `kind` `remote` a drive (`isRemote`, `remote`: server,
  owner, state, error), browsed by `my_role` like any. The drives list
  has them in their own "From other servers" section after the mirrors
  (`DrivesModel.remotes`), with whose they are and where
  (`RemoteDrive.fromLine`: "From alice@cloud.example", the server named
  apart when the address does not say it) and how they stand
  (`stateLine`: waiting for Accept, or the server's `error`). A pending one
  does not open (`DrivePlace(drive)` is nil) and offers Accept and Decline
  by its root (`DrivesModel.answer`, `Client.answerShared`:
  `POST /shared/{id}/accept`, `DELETE /shared/{id}`). Shared with me says
  "From <address>" for an item from another server (`SharedItem.fromLine`)
  and, while it is pending (`awaitsAnswer`), Accept and Decline instead of
  opening (`SharedModel.answer`). `DrivePlace.isRemote` (a remote drive's,
  or a shared item's whose node has `remote`): the pickers that start at
  the drives (share sheet, drop box) never offer one (`available`); Move
  inside one follows the role. `APIError` tells the federation refusals by
  `code`: `remote_unreachable` (502, `isRemoteUnreachable`: the upload
  queue retries it, the File Provider maps it to `serverUnreachable`),
  `remote_refused` (`isRemoteRefused`: not retried), `federation_off` and
  `server_refused` in words of the app's own; the others come only from
  sharing and invites (the web). The File Provider handles a remote
  drive's domain as any (capabilities by role, test
  `aRemoteDrivesItemsFollowTheirRole`).
- **Dependencies:** only swift-openapi-generator, -runtime, -urlsession
  and swift-http-types, exact versions in `Sources/OrganizerKit/Package.swift`.
- **Uploads** (`OrganizerKit/Uploads`): every upload is a job in the app
  group's container, sent by a background URLSession; under 4 MB one
  multipart POST, else the resumable protocol (`routes/uploads.rs`: create,
  then the rest in one PATCH, HEAD for the offset after a break), into a
  folder or the drop box (`drop_box=true`, each file its own job; the small
  ones of a drop go together in one multipart POST). The share
  extension copies what it gets into the container and leaves the sending
  to a background session of its own; the system wakes the app for its
  events (`handleEvents`), and the app carries the jobs on.
- **The token** is in the Keychain, access group = the app group
  (`group.eu.vardalabs.organizer`), so the extension reads it. A 401 signs
  out; Sign out revokes it (`DELETE /api/auth/device`), and Settings shows
  the user's email from `/api/auth/session`, which answers for a token too.
- **Notifications** (docs/M6D_DESIGN.md, `OrganizerKit/Notifications.swift`,
  `Sources/Organizer/Notifications.swift`): once signed in, the app asks
  for notifications and registers for remote notifications. The APNs token
  goes to `POST /api/notifications/subscriptions` as kind `apns`, lowercase
  hex, named as the device, again at every launch; a new token's row
  replaces the old one. Sign out removes the subscription before revoking
  the token. A push says nothing ("Something new for you",
  content-available), so the app asks `GET /api/notifications/news`: in
  the foreground the newest notice's words replace the push (a local
  notification), in the background the count to review and the badge are
  set, and a tapped notification opens the notice's tab. `aps-environment`
  is `development` until there is a team. Unsigned builds (`build:ios`) get
  no notifications at all; to try one, build ad-hoc signed for a simulator
  (`xcodebuild build -destination id=<udid>`, as `test:ios` does), sign in,
  then `xcrun simctl push <udid> eu.vardalabs.organizer payload.apns`.
- **Encrypted folders** (docs/E2E_SPEC.md, `OrganizerKit/Encryption`):
  the format with CryptoKit and CommonCrypto (`E2E.swift`), checked
  against `packages/e2e-crypto/vectors.json` (copied into the test bundle
  by `project.yml`). Unlocking (an alert on a locked folder's "Unlock to
  see what's inside") keeps the private key in the Keychain, access group
  the app group, so the share extension encrypts too; Lock (Settings) and
  sign out remove it. Folder keys are unwrapped per generation, in memory.
  Names are decrypted in `FolderModel` and sealed again for every change
  (new folder, rename, move); uploads encrypt the name and the contents to
  a staged file first (`Encryption.enqueue`), then go the usual way. The
  device's duties (`GET /api/encryption/pending`: keys for members
  waiting, a new generation) run after unlocking, at start and when an
  encrypted folder opens. Making encrypted folders is on the web.
  Shared from another Organizer server (`organizer-e2e`, docs/E2E_SPEC.md
  §12) an encrypted folder opens as a mirror's copy (its keys unwrapped
  with the answer's `folder_id`, names opened with `FolderModel.sourceID`),
  but may be written by its role: new names, renames, moves and uploads
  are sealed with the parent's `source_id` (`Encryption.sealingID`; in a
  tree sealed elsewhere, `FolderModel.sealedElsewhere`, a folder without
  one yet is asked again and refused while it has none:
  `notOnOwnersServer`). A 502 `remote_unreachable` for the keys is
  `EncryptionError.ownerUnreachable`. The owner's device wraps for a member
  on another server (`remote`) like any, by their `user_id`, the share's id
  (`Encryption.memberKeys`, new generations too).
- **Signing:** simulator builds have no team. Device builds and the App
  Store are a step of their own.

## Android app (`packages/android`)

Kotlin, Jetpack Compose (Material 3), Gradle with the committed wrapper
and a version catalog (`gradle/libs.versions.toml`, every version exact;
the JDK is mise's Temurin). Write `local.properties` (`sdk.dir=...`,
gitignored) for the Android SDK.

| Command | Use |
| --- | --- |
| `mise run build:android` | Debug APK (`app/build/outputs/apk/debug/`) |
| `mise run test:android` | JUnit tests: the upload client (MockWebServer), the device login, the token store, the view models (the drives and the folder picker too), the roles, the storage, the mirrors and the remote drives, the push notifications. **Must pass.** |
| `mise run check:android` | ktlint and Android lint. **Must pass.** |
| `mise run fix:android` | ktlint format |

All but `fix:android` run `gen:spec` first: the API client is generated by
the openapi-generator Gradle plugin from
`packages/backend/target/specs/openapi.json` into `core/build/` (never
committed).

- **Encrypted folders** (docs/E2E_SPEC.md, `:core` `e2e/`): the format
  with the JDK's AES-GCM and HMAC (our PBKDF2 and HKDF over them) and
  Tink's X25519 (Tink a direct dependency at the version UnifiedPush
  brings), checked against `packages/e2e-crypto/vectors.json` (the test
  task's system property `e2e.vectors`). `E2eKeys`: the private key
  unlocked with the passphrase, kept by `SessionStore` (Keystore-encrypted,
  cleared on sign out), folder keys per generation in memory, the
  device's duties (members waiting, new generations, `/encryption/pending`).
  `ApiFilesRepository` decrypts names and contents and seals names under
  an encrypted folder; uploads there are encrypted to a staged file first
  (`EncryptedUploads`); the share target refuses one while locked.
  Shared from another Organizer server (`organizer-e2e`, docs/E2E_SPEC.md
  §12, a node with `remote`) an encrypted folder opens with the mirrors'
  `MirrorSources` (keys unwrapped with the answer's `folder_id`, names
  opened with the parent's `source_id`), and its new names, renames and
  uploads are sealed with the parent's `source_id` (`MirrorSources.sealingFor`,
  `ApiFilesRepository.sealingId`: asked again while a folder made a moment
  ago has none, `NotOnOwnersServerException` while it still has none). A
  502 `remote_unreachable` for the keys is `OwnerUnreachableException`.
  The owner's device wraps for a member on another server like any, by
  their `user_id`, the share's id (`E2eKeys.newGeneration`, `missingKeys`).
- `:core`: the generated client, `SessionStore` (DataStore, the token
  encrypted with a key in the Android Keystore), `DeviceSignIn` (PKCE, the
  browser device login; the app's user comes from `GET /api/auth/session`
  with its token, and sign out is `DELETE /api/auth/device`),
  the generated client's `NotificationsApi` as `Api.notifications`,
  `UploadClient` (our own: plain multipart under
  4 MB, the resumable protocol of `routes/uploads.rs` from there, into a
  folder or the drop box), the live events (okhttp-sse). No Android UI.
- **Drives** (docs/M7A_DESIGN.md §3): the Files tab starts with the list
  of drives (`DrivesRoute`, `DrivesViewModel`: `GET /api/drives`, My
  drive first, each shared drive with "N members · size", then Shared
  with me from `GET /api/shared`); a drive opens its root (`root_id`) in
  the folder screen (`FilesRoute`, by node id, for every drive). Titles
  and paths start at the drive: `ApiFilesRepository` names a drive's root
  by the drive and gives each `Folder` its `driveName` (`location`, e.g.
  `Team/Plans`), from `ApiDrivesRepository`'s cached list; it drops a
  `.organizer` entry at a drive's root should one ever come. The folder
  picker (`FolderPickerModel`, the logic, JVM-tested) starts at the
  drives for the share target's "Place manually" and the drop box's
  placing (members may write in a shared drive; uploads go by folder id
  through the same `UploadWorker`), and a move starts in the node's
  folder and stays in its drive (the server refuses moves between drives).
- **Roles** (docs/M7B_DESIGN.md §1, §7, §11): the app follows `my_role`
  (`data/Roles.kt`, JVM-tested, as the server's `files/roles.rs`): a
  viewer gets no edit actions (no add button, no item menu); upload only
  may add files and folders, changes nothing, and its folder says it lists
  only what the user added (the server leaves the rest out); an editor and
  above rename, move and delete. `Folder.role` is the folder's own. The
  folder picker (a move, the share target, the drop box's placing) lets
  choose only a folder one may add to; a view-only folder can still be
  entered, as one below it may give more. No sharing screens yet.
- **Storage** (docs/M7C_DESIGN.md, `data/Storage.kt`, JVM-tested; no
  backend screens): a drive's `storage` (ready, unavailable with the
  server's reason and whether locked, moving). The drives list shows an
  unavailable one with its reason (a lock for a locked one) and does not
  open it (`opens`); a moving one says "Moving to <backend>…" and its
  folders are read-only (`Folder.movingTo`: no add, no item actions); the
  folder picker offers neither (`takesNew`). A drive on someone's own
  storage shows `ownerWarning` on its folders. `ApiException` keeps the
  body's `code`: `ApiException.of` makes a 503 `storage_unavailable` "This
  drive is not available now: <reason>."; a 409 `read_only` (the drive
  is being moved) is `readOnly` and retryable.
- **Mirrors** (docs/M7D_DESIGN.md §5, `data/Mirrors.kt`, JVM-tested; no
  mirror screens): a drive of `kind` `mirror` is listed in its own
  "Mirrors" group after the shared drives (`DrivesState.mirrors`), titled
  "<drive> · mirror" (`title`) with its state (`mirrorLine`, by the
  status's and `stopped`'s codes). Browsed read-only: its `my_role` is
  viewer and its folders' `Folder.mirror` makes them `readOnly`; the
  folder picker never offers one (`takesNew`). An encrypted folder in a
  mirror opens with its source's ids (`MirrorSources`: the keys asked by
  the top's `source_id`, names opened with the parent's).
- **Remote drives** (docs/M7E_DESIGN.md §5, §6, §10; `data/Remotes.kt`,
  JVM-tested in `RemotesTest`): a drive of `kind` `remote` (`isRemote`) is
  listed in its own "From other servers" group after the mirrors
  (`DrivesState.remotes`; `ownAndShared` leaves it out). Its row shows
  whose it is and where (`RemoteDrive.fromLine`: "From
  alice@cloud.example", the server named apart when the address does not
  say it) and how it stands (`stateLine`: waiting for Accept, or its
  server's `error`). A pending one has Accept and Decline by its root id
  and does not open until accepted (`opens`); once accepted it is browsed
  by `my_role` like any drive. Shared with me marks what came from another
  server (`SharedItem.fromLine`); while `remote.state` is pending
  (`awaitsAnswer`) the item has Accept (`POST /shared/{id}/accept`) and
  Decline (`DELETE /shared/{id}`) and does not open;
  `DrivesViewModel.accept`/`decline` go through `DrivesRepository` and
  read the list again. Pickers never put anything new on another server:
  `takesNew` leaves remote drives out, and a picker that starts at the
  drives (share target, drop box) hides and refuses folders with
  `Folder.remote` (from `Entry.remote`). A move inside a remote drive
  follows the role. `ApiException.of` words `remote_unreachable` (502,
  retryable, `remoteUnreachable`), `remote_refused` (not retried),
  `federation_off` and `server_refused` by their code; the other
  federation codes come only from sharing and invites (web only), so the
  server's words are kept. No sharing or access-list screens: `Grant.remote`
  is only in the generated model.
- `:app`: the screens (`ui/`, one view model each, plain constructor
  injection from `AppContainer`), `ShareActivity` (the share target), the
  WorkManager `UploadWorker` and `Staging` (a shared URI whose access
  cannot be kept is copied into the app's storage before the activity
  finishes).
- **Push notifications** (`app/.../push`, docs/M6D_DESIGN.md): UnifiedPush's
  connector (`org.unifiedpush.android:connector`, exact version in the
  catalog). After sign-in and at each start, the app registers with the
  user's distributor (the saved one, or the system default when no choice
  is needed) using the server's VAPID key (`GET /api/notifications`).
  `OrganizerPushService` hands each event to a `PushWorker`: a new endpoint
  becomes a `unifiedpush` subscription (name = `Build.MODEL`, keys
  p256dh/auth); an unregistration, the Settings switch turned off, or
  sign-out (before the token is revoked) removes it. A push carries no
  content: the worker reads `GET /api/notifications/news` (the generated
  `getNews`) and shows the newest notice not shown yet on the
  "Notifications" channel; a tap opens the app, at the drop box for
  `/drop-box`. `PushNotifications` holds the logic behind interfaces
  (`PushServer`, `Distributor`, `PushPrefs`) for JVM tests.
- View models take interfaces or functions, not Android types, so that
  JVM tests stand in for the server; `MainDispatcherRule` replaces Main.

## Desktop app (`packages/sync`, `packages/desktop`)

Full two-way sync for macOS, Windows and Linux (docs/M5_DESIGN.md, M5a),
and on-demand folders on macOS and Windows (docs/M5B_DESIGN.md, M5b).
`packages/sync` is the engine, a library with no UI; `packages/cloudfiles`
its on-demand folders on Windows (the Cloud Files API); `packages/desktop`
is the Tauri 2 app (`src-tauri`, a workspace member) and its window, a
small Vite/React app in the pnpm workspace, and `packages/desktop/macos`
the macOS File Provider extension and its tool (Swift, XcodeGen).

| Command | Use |
| --- | --- |
| `mise run test:sync` | The engine's tests, against the backend in-process, and the Cloud Files crate's (part of `mise run test`) |
| `mise run check:sync` | rustfmt and clippy of the engine and the Cloud Files crate, which is also linted for `x86_64-pc-windows-msvc` from any system (part of `mise run check`) |
| `mise run check:desktop` | rustfmt and clippy (`-D warnings`) of the engine and the app; oxfmt, oxlint and tsc of the window. **Must pass** when you touch either |
| `mise run test:desktop` | The app's Rust tests and the window's Vitest tests |
| `mise run fix:desktop` | cargo fmt, oxfmt and oxlint --fix |
| `mise run dev:desktop` | The app, rebuilt as its code changes (`tauri dev`; the window's dev server on :5174) |
| `mise run build:desktop` | This system's bundles, unsigned, into `dist/desktop` (`scripts/desktop-bundles.mjs` copies them out of the target directory). On a Mac it first builds the File Provider extension and its tool (`packages/desktop/macos/scripts/build.sh`, XcodeGen and xcodebuild, from Tauri's `beforeBundleCommand` in `src-tauri/tauri.macos.conf.json`, which copies them into the bundle). On a Mac the `.dmg`'s layout is set through Finder (AppleScript); where that is not allowed (a sandbox, a session without Finder automation), `CI=true` skips it |
| `mise run build:fileprovider` | The File Provider extension and its tool on their own (macOS); `CONFIGURATION=Debug` builds this Mac's architecture only, as CI does |

The app's tasks are not in `check` and `test`: on Linux they need Tauri's
system libraries (webkit2gtk 4.1 and the rest, see the CI job). Try the
app against a local backend: `DATA_DIR=$(mktemp -d) PORT=8123
cargo run -p organizer`, then sign in with `http://127.0.0.1:8123`.

- **The API client** is generated by progenitor at build time
  (`packages/sync/build.rs`) from `packages/backend/target/specs/openapi.json`
  (`gen:spec` first): it brings the 3.1 spec down to the 3.0 progenitor
  reads and keeps only the operations the engine calls (add one there when
  the engine needs it). Never committed. The multipart and resumable
  uploads, the downloads and the live events are written by hand on its
  reqwest client (`remote.rs`), as the apps do.
- **Encrypted folders** (`packages/sync/src/e2e.rs`, docs/E2E_SPEC.md):
  names decrypted in the pass's view of the tree (the state keeps them
  sealed), downloads decrypted, uploads encrypted to a scratch copy
  (`e2e-uploads/`), nothing moved across an encrypted folder's edge (trash
  and upload instead), locked folders hidden from the pass on both sides
  (never deleted), the device's key duties each round. The unlocked
  private key is in the credential store (`system.rs`), cleared on sign
  out or Lock; the settings' Encryption part uses the `unlock_encryption`
  and `lock_encryption` commands.
- **Engine layout** (`packages/sync/src`): `engine.rs` (the passes and the
  run loop), `state.rs` (the per-account SQLite state, `migrations/`),
  `remote.rs` and `local.rs` (the two sides), `names.rs` (non-portable
  names, conflict copy names), `ignore.rs`, `status.rs` (what the UI
  shows), `auth.rs` (PKCE, the loopback listener, the token exchange),
  `on_demand.rs` (the placeholders' provider and the `Fetcher` of their
  bytes).
- **Engine rules:** level-triggered, not an event queue: a pass compares
  the server's tree, the disk and what both agreed on last, and applies
  the difference, so a change made offline or while paused is still there
  at the next pass. Never overwrite: uploads go at the version last seen
  (`if_version`), and a change on both sides becomes a conflict copy
  `Name (conflict, <device>, YYYY-MM-DD).ext` beside the server's version.
  Never delete for good: to the server's trash and the system's
  (`trash`). Downloads go to a temporary file renamed into place. Plain
  uploads under 4 MB, resumable above (carried on after a break). A 410
  on the change feed means a full listing; a 401 signs out. A new
  behaviour comes with a test in `tests/sync.rs` (the backend in-process,
  behind a proxy the test can cut to play offline; a shared drive is made
  on the server's side, `state.files.create_shared_drive`, `root_node`,
  `set_member`, as the superadmin's endpoints want a step-up).
- **Drives** (docs/M7A_DESIGN.md): a synced folder is a drive's root (My
  drive's, a shared drive's) or a folder in one; the folder's `drive_id`
  is in the state (asked of the server when added, else at the next pass).
  The change feed covers all of the user's drives: only the changes under
  a synced folder are kept, so other drives' changes and the roots of
  drives not synced (`parent_id` null: joined or left) are let go. A
  synced folder whose top is gone from the mirror (its drive left or
  deleted: the root's `delete`, or a 404) is **unavailable**
  (`Status::unavailable`, the folder ids): not synced, nothing deleted
  here, until it comes back (its root listed anew) or the user removes
  it. At a drive's root a local `.organizer` (any case) is the server's
  own name, never sent (`Ignore::at_drive_root`). A move between two
  synced folders (of two drives, too: the server refuses those) is a
  delete in one and a new file in the other, as each folder is reconciled
  on its own. The window lists the drives (`listDrives` through the SDK)
  with their synced folders (`lib/drives.ts`); "Sync…" adds a drive's root
  or a folder browsed to in it, by default at `~/Organizer/<drive name>`
  (`~/Organizer/<folder name>` for a folder; a number added when taken:
  `local_place` in `commands.rs`, `defaultPlace` in `lib/drives.ts`).
  Folders synced before drives keep their place (My drive's top was
  `~/Organizer/Files`).
- **Roles** (docs/M7B_DESIGN.md §7): the mirror keeps each node's
  `my_role` (and its drive) as the listings say it; a node the feed told
  of keeps its own where it stays, else takes its parent's. Each pass asks
  every synced folder's top again: a role changed (which no change of the
  feed tells) lists it anew, and a 404 makes it unavailable. What the role
  does not allow is not sent and is listed in `Status::not_synced` (kept
  here as it is): something new or changed where the user may only view
  (`view_only`); a change, rename or move of what was added where they may
  only upload (`upload_only`; new files and folders go). A delete they may
  not make brings the server's copy back (`ActivityKind::Restore`).
  Something gone from the feed that the server no longer answers for and
  that is not in its drive's trash is no longer seen (limited access, a
  grant removed, upload only): left here with its unsent changes, out of
  the pass, never deleted (`hidden`), until it is seen again or the user
  removes it here. A trash that cannot be read (a placed share in another's
  drive) counts as no longer seen: nothing is deleted on a guess. The
  window shows each drive's role (`my_role` of `listDrives`) and each
  synced folder's (`role` in the app's state), and the not-synced list.
- **Storage** (docs/M7C_DESIGN.md §2, §4): each pass lists the drives once;
  a drive not `available` (its backend fails, or is locked), or with a
  `moving_to`, makes its synced folders wait (`Status::waiting`: the folder,
  `unavailable` or `moving`, the reason): not reconciled, nothing deleted on
  either side, and what changed here since the last agreement (new, changed
  by size or time, deleted) listed in `not_synced` with that `Held` and the
  server's `reason`; sent at the first pass after. Found during a pass
  instead (a 503 `code` `storage_unavailable` is `Error::StorageUnavailable`,
  a 409 `code` `read_only` is `Error::ReadOnly`, never a version
  conflict), the folder waits from there.
  Both are `Error::waits()`, like being offline (a resumable upload and an
  encrypted scratch copy are kept). The window's Synced folders shows a
  drive's storage (`lib/drives.ts`: the owner's warning, "Not backed up by
  the server", why it does not sync now, a lock for a locked one), a
  waiting folder's badge, and the sync page the folders waiting.
- **Mirrors** (docs/M7D_DESIGN.md §5): a mirror is a drive like any, synced
  read-only by the viewer's rules above (`my_role` viewer on all of it):
  nothing made or changed here is sent, a delete here is undone (test:
  `a_mirror_syncs_read_only`, its mirror made on the server's side as
  `POST /mirrors` makes it, on a backend of the user's own whose S3 an
  in-memory stand-in is, `Backends::stand_in_own`). An encrypted folder
  in one has no keys of its own: `e2e.rs` asks them by the top's
  `source_id` when its own id has none, unwraps the mirror's kept keys
  (answered by the copy's id) with the answer's `folder_id`, and opens
  names with the parents' sources (`E2e::sealed_for`). The window's Synced folders
  lists the mirrors in their own group (`driveRows`' `mirrors`), titled
  "<drive> · mirror" (`driveLabel`) with their state (`mirrorState`, by
  the status's and `stopped`'s codes), and Sync… of one says it is a
  read-only copy.
- **Remote drives** (docs/M7E_DESIGN.md §5, §6): a drive shared from
  another server (`kind` `remote`) is a drive like any to the engine: its
  bytes go through the user's own server, which follows the other one and
  writes the changes into the feed, so it syncs by `my_role` with the
  rules above (test: `a_drive_from_another_server_syncs_by_its_role`, two
  servers in the test's process, federation opened through the
  superadmin's endpoint after a real second factor, `organizer::totp::code_at`,
  each the other's contact by an invite, a shared drive given as a viewer,
  live notices from `outgoing::run_live`). The window's Synced folders
  lists them in their own group, "From other servers" (`driveRows`'
  `remotes`), with whose they are and where (`remoteOrigin`) and how they
  stand (`remoteState`: waiting for Accept, its server's error); a pending
  one has Accept and Decline (the SDK's `acceptShared`, `declineShared`,
  by its root) instead of Sync… (`syncable`), and no storage notes.
  An encrypted folder in one (`organizer-e2e`, docs/E2E_SPEC.md §12) is
  opened as a mirror's copy: its keys asked by its id here and unwrapped
  with the answer's `folder_id` (the owner's), names opened with the
  parents' `source_id`. Unlike a mirror the role may allow writing: new
  names, renames and moves are sealed with the parent's `source_id`
  (`E2e::sealing_for`), asked again while a folder made a moment ago has
  none, and not sent while it still has none. A 502 `remote_unreachable`
  is `Error::StorageUnavailable` ("The owner's server is not reachable"):
  its folders wait. The owner's device wraps for a member on another
  server (`remote`) like any, by their `user_id` (the share's id), new
  generations too (test:
  `an_encrypted_folder_from_another_server_opens_and_is_written_with_its_ids`).
- **On-demand folders** (docs/M5B_DESIGN.md): a folder's `mode` is `sync`
  or `on_demand`. On Windows the engine keeps an on-demand folder itself,
  through `organizer-cloudfiles`' `Placeholders` (a trait; Windows'
  `CloudFiles` is the one implementation): the folder is a sync root
  (registered with `StorageProviderSyncRootManager`, which gives Explorer's
  navigation pane and its menu items), a file of the server becomes a
  placeholder (identity: the node's id and version) instead of a download,
  the system's fetch callback is answered with range downloads at that
  version (`Fetcher`), a placeholder is never hashed (that would download
  it: its identity says whose it is, its in-sync state whether it
  changed), what was sent or downloaded is adopted as a placeholder in
  sync, a pinned one is hydrated and an unpinned one freed, and one not
  downloaded is removed rather than trashed when the server deleted it.
  Elsewhere the engine leaves an on-demand folder alone (macOS: the File
  Provider extension keeps it). Its tests run the same passes with a fake
  `Placeholders` in `tests/sync.rs`.
- **Encrypted folders are never on demand**: a placeholder's bytes come
  straight from the server when the system asks (Windows' fetch, the
  macOS extension), sealed. `Engine::may_be_on_demand` refuses an
  encrypted folder or one inside one (`ENCRYPTED_ON_DEMAND`; the
  `add_folder` command asks before it makes a File Provider domain, and
  Add a folder offers only Sync for one); the encrypted folders inside an
  on-demand folder are left out of it, hidden as a locked one is (never
  deleted, nothing sent), and OrganizerKit's `ProviderClient` leaves them
  out of the listings and the working set.
- **`packages/cloudfiles`**: the Windows calls. Its pure parts (the
  identity's bytes, the 4 KiB-aligned ranges and pieces of a fetch, what a
  pin state asks for) are tested on any system; `cfapi.rs`, built on
  Windows only, is the one module with unsafe code (`#![allow(unsafe_code)]`
  there, the crate denies it, every other crate forbids it): the `windows`
  crate's Cloud Files functions are all `unsafe`, and the fetch callback is
  an `extern "system"` function. Only the features it needs of `windows`,
  as a `cfg(windows)` dependency. `check:sync` lints it for Windows from a
  Mac or Linux (the target's std is in `rust-toolchain.toml`); the Windows
  CI job runs it.
- **The macOS File Provider extension** (`packages/desktop/macos`,
  `project.yml`; generated project, plists and entitlements not
  committed): `FileProvider/` (an `NSFileProviderReplicatedExtension`, a
  domain per on-demand folder whose identifier is the folder's id (a
  drive's root id for a whole drive, named as the drive); a thin
  layer over OrganizerKit's `ProviderClient`), and the tool
  `organizer-fileprovider` (`Helper/`: `add`, `remove`, `remove-all`,
  `signal`, `session`, `forget`), which the Rust app runs from
  `Contents/MacOS` (`src-tauri/src/file_provider.rs`): File Provider is an
  Objective-C API and the app forbids unsafe code, so no FFI. The app adds
  a domain when an on-demand folder is added (the engine lists the folder
  at the domain's place and leaves it alone), removes it with the folder
  and all at sign-out, keeps the extension's session (the server and token,
  on the tool's stdin, into the Keychain group the two share), and signals
  the working sets when the live events say something changed (half a
  second after the first of a burst). The working set follows the change
  feed, its cursor the sync anchor (410: listed again). Swift's
  `libswiftCompatibilitySpan.dylib` goes in the app's `Contents/Frameworks`
  for macOS before 26.
- **The app** (`src-tauri/src`): `lib.rs` (the state, the engine's
  status and events to the window, the tray and notifications),
  `commands.rs` (what the window invokes), `tray.rs`, `settings.rs` (the
  account and the user's choices in `settings.json`; the synced folders
  are in the engine's state), `system.rs` (the token in the credential
  store through `keyring`, never in a file or the web view; the
  computer's name). Plugins: single-instance, autostart, notification,
  opener. `unsafe_code` is forbidden here too.
- **The window** (`packages/desktop/src`) imports the web app's components
  (`@/` is `packages/frontend/src`): never copy one; a component both need
  goes in the frontend. Its SDK calls go through the app's `api` command
  (`src/lib/transport.ts`), which adds the token. Its own code: the
  pages, `lib/desktop.ts` (the app's state and commands, typed as in
  `lib.rs`: change both together), `lib/status.ts` and `lib/drives.ts`
  (with Vitest tests).
- **Signing:** none yet: the bundles are unsigned. The File Provider
  extension does not load until the app, the extension and its tool are
  signed with an Apple team: set `DEVELOPMENT_TEAM` (and the signing style
  and identity) in `packages/desktop/macos/project.yml`, where the
  entitlements are (app sandbox, network client, the app group and the
  Keychain access group of the team), and sign the Tauri app with the same
  team. `scripts/build.sh` builds unsigned while the team is empty.

## Before you start a milestone's feature

- **AI:** before any work on the AI integration (M2: the model and
  embeddings client, the drop box pipeline, the AI settings), ask the user
  how they want the use of AI configured. Do not start from the
  architecture document alone.
- **Schema and endpoints of M1:** propose them to the user for review
  before writing them (see `docs/ARCHITECTURE.md` §8).
- **Frontend pages:** present each page to the user and get their approval
  before creating it. The UI is stock shadcn/ui (Base UI, Nova) with the
  `sidebar-07` layout (`packages/frontend/AGENTS.md`).
- **Dependencies and generated code:** add or generate them with the
  command the tool's own docs recommend (`pnpm add`, `pnpm dlx shadcn@latest
  add`, `cargo add`, ...), never by editing a manifest or copying files in
  by hand.

## Conventions

- **Use the community's crates.** When a well-known, maintained crate (or
  npm package) solves the problem, use it rather than writing the same
  thing ourselves. Propose a new one first (name, version, why) and add it
  only once it is approved. Write it ourselves only when nothing
  maintained fits, and say why in a comment.
- **Simple.** No ORM, no abstraction layers. Comment the *why*, not the what.
- **Versions live in one place each**, read by mise
  (`idiomatic_version_file_enable_tools`): Node (the current LTS) and pnpm
  in the root `package.json` (`devEngines.runtime`, `packageManager`), Rust
  in `rust-toolchain.toml`, the other tools (zig, cargo-zigbuild, watchexec,
  osv-scanner, Trivy) in `mise.toml`. Crates are pinned to the exact
  version in `Cargo.toml`. The image builds nothing: it is an exact Alpine
  release (`alpine:3.x.y`, with `apk upgrade`) and the static musl binary
  built outside it (`scripts/build-linux.sh`). Its healthcheck is
  `organizer healthcheck`.
- **Configuration:** environment variables only for what is needed before
  the database opens, unprefixed (`PORT`, `DATA_DIR`, `DATABASE_URL`,
  `FILES_DIR`, `PUBLIC_URL`, `TRUST_PROXY`, `SWAGGER_UI`, `TOTP_REQUIRED`, `OIDC_*`,
  `VAPID_PRIVATE_KEY`, `APNS_*`, `SECRET_KEY_FILE`, `BACKENDS_ALLOW_PRIVATE`,
  `OCM_KEY_FILE`, `FEDERATION_ALLOW_PRIVATE`, `RUST_LOG`; listed in the
  README). Everything user-facing is stored in the database. A new variable
  goes in the README's table.
- **CI** (`ci.yml`) verifies and ships nothing: dev builds only, one per
  system (Linux, the image's musl, macOS, Windows), every job side by side,
  for a whole run within 15 minutes. Release builds, the desktop bundles
  included, are `release.yml`'s, on a release. The jobs that need only the
  OpenAPI spec download it from the `spec` job and run their tasks with
  `mise run --skip-deps <task>` (the flag before the task), since mise
  would run `gen:spec` again on a fresh runner. Caches (Rust's, Xcode's,
  Gradle's) are saved from main.
- **Dependency updates and security fixes** (`.github/dependabot.yml`):
  Dependabot's pull requests are `fix(deps)` for what ships (crates, the
  frontend's dependencies, the base image) and `chore(deps-dev)` for the
  frontend's devDependencies; `ci` for actions. Routine updates wait 5 days
  (`cooldown`), security updates do not. Minor, patch and security updates
  merge on their own once CI passes (`dependabot-auto-merge.yml`); majors
  wait for a person. pnpm's `minimumReleaseAge` (1 day) must stay shorter
  than the cooldown. `security.yml` runs `mise run audit` and scans the
  published image every day; a finding to accept goes in
  `osv-scanner.toml` with its reason.
- **Generated, do not edit by hand:** `CHANGELOG.md` and the version fields
  (release-please), `packages/frontend/src/routeTree.gen.ts` (TanStack
  Router), `pnpm-lock.yaml` and `Cargo.lock` (use the
  package managers), and `packages/sdk/src` (`mise run gen:sdk`).
- **License:** Hippocratic License 3.0. No GPL or AGPL dependencies (MIT,
  Apache and the like are fine); check a new dependency's license.

## Git and releases

Commit messages are **Conventional Commits** (`feat:`, `fix:`, `perf:`,
`ci:`, `docs:`, ...): release-please reads them to bump the version and
write the changelog. So are **pull request titles**: pull requests are
squash merged, the commit on main takes the title, and release-please skips
one that is not conventional. GitHub fills a new pull request's title from
the branch name: rewrite it. The `PR title` workflow fails a title that
does not parse. Do not commit unless asked, and work on a branch, not
`main`. Releases are automated; never tag by hand.
