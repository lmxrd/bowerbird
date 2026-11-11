# M5b design proposal — on-demand files

Status: **accepted 2026-11-08** (§6), **built 2026-11-08** (§7): macOS
and Windows, unsigned. The macOS extension is built, embedded in the
app's bundle and its logic tested; it loads once the project has an Apple
team (§7). The Windows code is linted for Windows from a Mac and runs in
the Windows CI job; neither system's on-demand folders were tried by hand
yet. The second
step of M5 ([M5_DESIGN.md](M5_DESIGN.md) §1): folders whose files show in
the system's file manager with their name, size and icon, but are
downloaded only when opened, and can be freed again. macOS first, then
Windows; Linux has no such API and keeps full sync. The questions are in
§6.

## 1. What the user sees

- A synced folder's mode can be **On-demand** (Settings → Synced folders,
  the mode the M5a screen already shows). Its files appear in Finder (in
  the sidebar under Locations, "Organizer") or Explorer (a navigation-pane
  entry), with a cloud icon until downloaded.
- **Opening** a file downloads it first (a progress bar in Finder or
  Explorer); then it stays.
- **Free up space** / **Always keep on this device**: the system's own
  menu items on a file or folder.
- Changes both ways, conflicts and deletes to the trash work as in full
  sync; the drop box folder stays a full-sync folder.

## 2. macOS: a File Provider extension

macOS allows on-demand files only through **File Provider**
(`NSFileProviderReplicatedExtension`): an app extension inside the app
bundle, written in Swift, which the system runs when Finder asks for an
item, a listing or changes. It cannot be Rust or part of Tauri's process.

- The extension reuses **OrganizerKit** from the iOS app (the generated
  Swift client, the session, the resumable uploads): one Swift client for
  both. The same extension can later give the iOS app its place in the
  Files app.
- It talks to the server itself: **enumerations** are folder listings, the
  **working set** follows the change feed (`/changes?since=`, its cursor
  kept as the extension's sync anchor), **fetching** a file downloads it,
  **creating, modifying, deleting** upload, move or trash it, at the
  item's version (a version that moved on is a conflict copy, as in M5a).
- The Tauri app adds and removes the File Provider **domain** for an
  on-demand folder, and tells the extension (through the shared app group)
  that new changes came in from the live events, so Finder updates at once.
- The token is shared through the Keychain access group, as the iOS share
  extension does.
- **Signing**: a File Provider extension does not load unless the app and
  the extension are signed with an Apple team and its entitlements (app
  group, File Provider testing mode for development). A free Apple
  account's "Personal Team" is enough to run it on your own Mac; a paid
  Developer ID is needed to give the app to others (notarised).

## 3. Windows: the Cloud Files API

Windows' **Cloud Files API** (`cfapi`, the one OneDrive uses) is callable
from Rust through Microsoft's `windows` crate, inside the Tauri app's own
process: register a **sync root** for the folder, create **placeholders**
from listings, answer **fetch-data callbacks** with downloads, and report
local changes with the M5a engine's watcher. Dehydration (freeing up
space) is the system's. No signing is needed to run it locally.

## 4. Order and testing

1. macOS: the extension target in the Tauri bundle (Tauri can embed an
   extension built by `xcodebuild` from a small XcodeGen project beside the
   iOS one), the domain management in the app, then the extension's
   enumeration, fetching and changes. Tests: OrganizerKit's enumerator and
   change logic against a stub server (as the iOS tests do); a manual run
   on this Mac with your team.
2. Windows: the sync root and placeholders, fetch callbacks, changes.
   Tests: the placeholder logic in unit tests; the API itself only on the
   Windows CI runner, since it cannot run on a Mac.

## 5. What changes elsewhere

- `packages/ios` gains a macOS target for OrganizerKit (it is already a
  Swift package; the macOS platform is added).
- `packages/desktop` gains the extension in its macOS bundle and the
  Windows module; the settings' mode becomes selectable.
- No server change.

## 6. Decided (2026-11-08)

1. **No Apple team yet**: the macOS File Provider extension is built and
   unit-tested now, unsigned; it runs once a team id is set in the
   project (then also iOS's Files app, later).
2. **The `windows` crate** (Microsoft, MIT or Apache-2.0) with the Cloud
   Files features, built on Windows only.
3. **macOS and Windows** both now.

## 7. Built (2026-11-08)

- **Mode**: chosen when a folder is added (Settings → Synced folders → Add
  a folder: Sync or On-demand, on macOS and Windows; Linux shows no
  choice), shown as the folder's badge. To change it, the folder is
  removed and added again (an on-demand folder's place is the system's on
  macOS, and turning one into the other moves or downloads everything).
- **Windows** (`packages/sync`, `packages/cloudfiles`): the engine keeps
  an on-demand folder with the same passes as a synced one, through a
  `Placeholders` trait whose Windows implementation calls the Cloud Files
  API (`windows` 0.62.2, `cfg(windows)`, only the features it needs). The
  sync root is registered with `StorageProviderSyncRootManager` (Explorer's
  navigation pane and menu items) and connected to a fetch callback; the
  population policy is "always full", the engine making every placeholder
  from the server's tree (the change feed does not say a file's size, so
  it is asked for when needed; the mirror keeps it from the listings). The
  unsafe calls are all in `packages/cloudfiles/src/cfapi.rs`, the crate
  denies unsafe code elsewhere and every other crate forbids it. Pins:
  "Always keep on this device" hydrates at the next pass, "Free up space"
  dehydrates (the system sets the attributes; the provider acts on them).
- **macOS** (`packages/desktop/macos`): the extension and the
  `organizer-fileprovider` tool, an XcodeGen project beside the Tauri app
  (it ships in that bundle and is built by its `build:desktop`, not by the
  iOS project), on OrganizerKit with the macOS platform added (linked
  statically). The Rust app runs the tool (`Contents/MacOS`) to add and
  remove domains, signal the working sets on live events and hand the
  extension its session, rather than calling Objective-C through FFI, which
  its `forbid(unsafe_code)` rules out. The tool and the extension share the
  session through the Keychain's default access group (the team's).
- **Encrypted folders** (M6e) are never on demand: their placeholders
  could only hold the server's sealed bytes. Refused when added, left out
  of an on-demand folder that holds one (docs/M6E_DESIGN.md §8).
- **Verified**: the engine's on-demand passes against the backend
  in-process with a fake `Placeholders` (placeholders made, updated,
  hydrated when pinned and freed when unpinned, edits sent and adopted,
  renames recognised without a download, deletes), the fetch's ranges at
  a version, the Cloud Files crate's pure parts, and its Windows code with
  clippy for `x86_64-pc-windows-msvc`; OrganizerKit's File Provider logic
  (items, the working set's scope, listings, the feed and its 410,
  downloads, modifications at a version and conflict copies, deletes)
  against a stub server; the extension and the tool built and embedded in
  the `.app` and `.dmg` by `build:desktop`.
- **Not verified** (needs a signed app, or Windows): the extension loaded
  by macOS and used from Finder; the Windows sync root, placeholders and
  fetches on a real Windows (only CI compiles and runs the tests there).
