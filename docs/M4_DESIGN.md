# M4 design proposal — the mobile apps

Status: **built 2026-11-07** (§8 decisions). Both apps build, pass their
checks and tests (iOS 44, Android 64) and CI jobs run them. Tried against a
real server: on the iOS simulator, signing in through the browser, uploads
(the resumable path byte-identical), Quick Look and sign-out; on the
Android emulator, signing in, a resumable upload, sharing a link to the drop
box, and sign-out. Not yet tried: the share sheets invoked from other apps
with files, background and interrupted transfers on a real device, iOS 18
and physical devices, release builds and store publishing (they need your
developer accounts). Drop-box files of 4 MB or more go through the
resumable uploads too (`drop_box=true`, added 2026-11-07), so the apps
take drop-box files of any size. It covers
milestone M4 of [ARCHITECTURE.md](ARCHITECTURE.md) (§6.2 there): native
iOS (Swift, SwiftUI) and Android (Kotlin, Jetpack Compose) apps with a
share target and the drop box's review queue. The questions are in §8.

Decided already (ARCHITECTURE.md): native apps, not a shared toolkit;
clients generated from the OpenAPI spec (which is not committed); the
device login (M1) for signing in; resumable uploads with our own client on
both platforms (no 104 from hyper, §2 there); in-app live events (SSE)
only, no push yet.

## 1. What the apps do

| Screen | What |
| --- | --- |
| **Sign in** | The server's address, then the device login in the system browser (ASWebAuthenticationSession, Custom Tabs): password, single sign-on and two-factor all work as on the web, and the app gets its own token, listed and revocable under Settings → Devices |
| **Files** | Browse folders; open a file (Quick Look / the system's viewer, downloaded to a cache); upload from Photos or Files; new folder, rename, move, delete (to the trash); pull to refresh |
| **Drop box** | The items waiting and placed, as on the web: each with its proposal (folder, name, reason, confidence), and Confirm, Place elsewhere, Reject, Undo; add files, photos or a link. A badge counts what waits for review |
| **Share sheet** | From any app, files, photos or a link: **Drop box** (with an optional note, and the per-item options the user's settings allow: read contents, keep a folder whole, what to do with a link) or **Place manually** (pick a folder) |
| **Settings** | The server, the account, sign out (which revokes the token) |

Not in M4: the trash, versions and backups (on the web), search (M4.1, it
is one endpoint), push notifications, offline files (M5's sync engine).

Live events: while the app is open it follows `/api/events` (SSE),
so the drop box and the folder on screen update as on the web.

## 2. Uploads

Both apps speak the resumable upload protocol (`/api/uploads`, interop
version 6) with their own small client, so a large upload survives a
dropped connection and continues from its offset.

- **iOS**: background `URLSession` tasks for each chunk, so an upload goes
  on when the app is in the background. The share extension (small memory
  limit, killed early) copies what it receives into the app group's
  container and hands the upload to a background session the main app
  owns; it never uploads in the extension's own memory.
- **Android**: a `WorkManager` job per upload (a foreground service with a
  progress notification for large files), so it survives the app being
  closed. The share activity enqueues the work and finishes at once.

Files under 4 MB use the plain upload (`POST /files/{id}/files`,
`POST /drop-box/files`) to keep small things simple. Larger ones are
resumable whether they go into a folder or the drop box: the creation
takes `drop_box=true` with the name, the note and the read-contents
choice, and the complete upload becomes a drop-box item as a multipart
drop would (no 100 MB limit).

## 3. Signing in and the token

The device login (M1): `GET /api/auth/device/authorize` with a PKCE
challenge and `redirect_uri=organizer://auth`, in the system browser; the
app takes the one-time code from the redirect and exchanges it at
`POST /api/auth/device/token`. The token (an API key of kind `device`) is
kept in the **Keychain** (shared with the extension through an access
group) and in Android's **Keystore**-encrypted storage. A 401 signs the
app out.

## 4. The code

```
packages/ios/        Organizer.xcodeproj made by XcodeGen from project.yml:
                     the app, the share extension, a Swift package
                     (OrganizerKit: the generated client, the upload
                     client, the session) shared by both, and tests
packages/android/    Gradle (Kotlin DSL, a version catalog): :app (with the
                     share activity), :core (the generated client, the
                     upload client, the session), tests
```

- **Clients from the spec.** iOS: Apple's swift-openapi-generator as a
  build plugin, reading `packages/backend/target/specs/openapi.yaml`
  (`mise run gen:spec` first). Android: the openapi-generator Gradle
  plugin (`kotlin` with OkHttp and kotlinx.serialization) into `:core`'s
  build directory. Neither is committed, as the web's.
- **XcodeGen** keeps the Xcode project as a small YAML file (`project.yml`)
  instead of a `.pbxproj` nobody can review; `mise run gen:ios` makes it.
- **Versions** live in one place each, exact: Swift packages in
  `project.yml`, Android libraries in `gradle/libs.versions.toml`, the
  tools (XcodeGen, Gradle, Java) in `mise.toml`. New versions wait a day.
- **mise tasks**: `build:ios`, `test:ios` (the simulator), `build:android`,
  `test:android`, and `check` grows SwiftFormat/SwiftLint-free checks:
  `swift format lint` (in the toolchain) and ktlint through Gradle.

## 5. Look

Each platform's own: SwiftUI's lists, navigation stacks and sheets;
Material 3 on Android. The web app's words and order, not its styling.
The screens are mocked up for approval before they are built (§8).

## 6. CI and releases

CI builds and tests both apps on every pull request (iOS on a macOS
runner, Android on Linux), after `gen:spec`. M4 does not publish to the
App Store or Google Play: that needs your developer accounts and signing
keys, and is a step of its own. Debug builds install on a simulator or
emulator, and a signed Android APK can be attached to a release later.

## 7. Order of work

1. Nothing new in the backend: links, files, the items (filtered by
   status, for the badge) and the live events are all there.
2. iOS and Android **in parallel** (one subagent each, the same plan):
   project and client generation → sign in → uploads → **the share
   target** → files → drop box review → settings.
3. CI jobs; README and AGENTS sections.

Each step committed and tested on its own; each app's tests run its
session, upload client and view models against a stubbed API, and one
smoke test signs in and uploads against a real server.

## 8. Decisions (2026-11-07)

1. **Tools and libraries**, as recommended (the comparison was made in a
   side conversation): XcodeGen (mise), swift-openapi-generator with its
   runtime and URLSession transport, URLSession, the Keychain, `swift
   format`; Gradle (the committed wrapper) with the Android Gradle plugin,
   Kotlin, Jetpack Compose with Material 3 (its Expressive components where
   they are stable), Navigation Compose, androidx.browser, WorkManager, the
   openapi-generator Gradle plugin (`kotlin`, OkHttp with okhttp-sse,
   kotlinx.serialization), the Android Keystore with DataStore, ktlint; a
   Temurin JDK (mise). Sign-in stays the browser device login: password,
   two-factor, single sign-on, and passkeys once the server has them, all
   without app code (a native passkey screen cannot work for a
   self-hosted server's domain).
2. **The app identifier**: `eu.vardalabs.organizer`, for now (the name
   will change).
3. **Lowest versions**: iOS 18, Android 10 (API 29).
4. **The screens**: approved as mocked up. **Sharing from any app into
   Organizer comes first**, as soon as sign-in and uploads work: it is the
   feature that matters most.

Passkeys on the server (webauthn-rs) were added alongside (2026-11-07): a passkey signs in on its own, the password stays as a fallback; the apps get them through the browser sign-in.
