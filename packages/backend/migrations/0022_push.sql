-- Push notifications (docs/M6D_DESIGN.md): the server's VAPID key, each
-- user's devices (browsers, the Android app through UnifiedPush, the iOS
-- app through APNs), what each user wants to hear about, and the notices
-- sent, which the apps ask for after a content-free wake-up.

-- The VAPID key pair, made at first start (`VAPID_PRIVATE_KEY` wins over
-- it): the private key, base64url, its 32 raw bytes.
CREATE TABLE push_keys (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  vapid_private_key TEXT NOT NULL
);

CREATE TABLE push_subscriptions (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('web', 'unifiedpush', 'apns')),
  -- What the user sees in the list: "Firefox on Linux", "Pixel 8".
  name TEXT NOT NULL,
  -- The push service's URL (Web Push), or the device token (APNs, hex).
  endpoint TEXT NOT NULL UNIQUE,
  -- Web Push only: the client's P-256 key and auth secret, base64url.
  p256dh TEXT,
  auth TEXT,
  created_at TEXT NOT NULL,
  last_notice_at TEXT
);
CREATE INDEX push_subscriptions_user ON push_subscriptions (user_id);

-- What each user is told about; no row means everything on.
CREATE TABLE notification_settings (
  user_id BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  review INTEGER NOT NULL DEFAULT 1,
  shared INTEGER NOT NULL DEFAULT 1,
  backup INTEGER NOT NULL DEFAULT 1,
  -- When the last notice of items to review was sent: at most one a
  -- quarter hour.
  review_notified_at TEXT
);

-- The notices sent, kept a week: what "what's new" answers.
CREATE TABLE notices (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('review', 'shared', 'backup')),
  -- Items to review: how many there were.
  count INTEGER,
  created_at TEXT NOT NULL
);
CREATE INDEX notices_user ON notices (user_id, created_at);
