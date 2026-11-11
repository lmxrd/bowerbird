# M6d design proposal — push notifications

Status: **built 2026-11-08** (§6 decisions, §7 what was built). The fourth
step of M6 ([M6_OUTLINE.md](M6_OUTLINE.md)): a notice reaches you when the
app is not open — "3 items to review", "Kari shared Recipes with you".

## 1. One protocol: Web Push

**Web Push** (RFC 8030, encrypted with RFC 8291, the server identified by
VAPID, RFC 8292) is the open standard behind browser notifications, and
also what **UnifiedPush** speaks on Android: a distributor app the user
picks (ntfy, NextPush, or Google's FCM through a gateway) receives and
wakes the app, with no Google account needed. So one implementation in the
server reaches:

- **Browsers** (the web app; on iPhone too, when it is added to the home
  screen, iOS 16.4 and later);
- **The Android app**, through UnifiedPush;
- **The desktop app** has no push; it is open in the tray, and shows the
  system's notifications from the live events as now.

- **The iOS app**, through **APNs** (decided: set up now, before the app
  has its final name or an Apple team). The server sends APNs requests
  (HTTP/2, a token signed with the team's `.p8` key) once the admin gives
  the key: `APNS_KEY` (the `.p8` file's contents), `APNS_KEY_ID`,
  `APNS_TEAM_ID`, and the topic `APNS_TOPIC`, the app's bundle id
  (`eu.vardalabs.organizer` for now). The app registers for remote
  notifications and sends its device token. Until there is a team, it is
  tested on the simulator (`simctl push`) and against a stand-in APNs.

The server sends only a **wake-up with no content** ("something new for
you": the payload is encrypted anyway, but nothing about files leaves the
server); the app or browser then asks the server what it is, so the text
of the notice never passes through Google, Mozilla or ntfy.

## 2. What notifies

Each user chooses, per kind, in Settings → Notifications:

- items **waiting for review** in the drop box (at most one notice per
  quarter hour, with the count);
- something **shared with you** (M6b);
- a **failed backup** (admins).

## 3. Devices

Each browser or phone that turns notifications on is a **subscription**
(its push endpoint and keys), listed in Settings → Notifications with its
name and when it last got one; removable. An endpoint the push service says
is gone (404/410) is removed.

## 4. Keys

The VAPID key pair is made at first start and kept in the database
(`VAPID_PRIVATE_KEY` may set it, for setups that keep keys in code). Push
services see the server's public key and its `PUBLIC_URL` as contact, which
it needs.

## 5. Crates and libraries

- **web-push-native 0.5.0** (MIT or Apache-2.0, RustCrypto: aes-gcm, p256,
  hkdf; no OpenSSL): builds the encrypted Web Push request, which our
  reqwest sends. Exact version at build time.
- Android: **UnifiedPush's connector** (`org.unifiedpush.android:connector`,
  Apache-2.0) in the app.
- The web: the browser's own Push API and a service worker; no package.

## 6. Decided (2026-11-08)

1. **Web Push** for browsers and the Android app (UnifiedPush), content-free
   wake-ups; **and APNs for the iOS app now**, configured by the admin once
   there is a team and key.
2. **web-push-native** and UnifiedPush's Android connector are added. For
   APNs' signed token the server uses the ES256 signing web-push-native
   already brings (its p256 / jwt dependencies), at the same versions.
3. **The page** (Settings → Notifications) as mocked up. APNs is set up
   with variables only (`APNS_KEY`, `APNS_KEY_ID`, `APNS_TEAM_ID`,
   `APNS_TOPIC`); a settings card for it can follow with a mockup.

## 7. Built (2026-11-08)

- **Server** (`push.rs`, `routes/notifications.rs`, migration 0022):
  `/api/notifications` (switches, devices, the VAPID public key, whether
  APNs is set up), `/notifications/settings`, `/notifications/subscriptions`
  (kinds `web`, `unifiedpush`, `apns`; the same endpoint again replaces its
  row; at most 20) and `/notifications/news` (the count to review and the
  last day's notices, worded by the server). `push::notify(state, user,
  kind)` is what the sharing code (M6b) calls with `Kind::Shared`. Items to
  review are counted 5 s after a drop-box change, one notice per quarter
  hour at most, only when something changed since the last. A backup job
  that failed for good tells the admins. Push endpoints go through the
  links' guard (https, public addresses; `LINKS_ALLOW_PRIVATE` lifts it).
  No new crate but web-push-native: its re-exports (jwt-simple, p256,
  ct-codecs) sign the APNs token, and reqwest's and axum's (tests only)
  `http2` features speak HTTP/2.
- **Web**: Settings → Notifications as mocked up, and `public/sw.js`, which
  fetches what's new on a push and opens its page on a click.
- **Android**: UnifiedPush's connector 3.3.5; registration after sign-in
  (and a "Notifications" switch in Settings); a push shows the newest
  notice not shown yet.
- **iOS**: APNs registration after sign-in; the push is a fixed alert
  ("Organizer / Something new for you", content-available) with no
  notification service extension: in the foreground the app shows the
  notice's own words instead, in the background it sets the badge, and a
  tap opens the notice's tab. Tried on the simulator with `simctl push`.
- **Not tried**: a real push service end to end (the browser pane used
  blocks notifications and service workers; Android needs a distributor
  app and a public https endpoint; APNs needs a team and key).
