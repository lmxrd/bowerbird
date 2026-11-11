-- A team's managers act on accounts too (docs/M7B_DESIGN.md §5): adding
-- someone to their team, taking them out, changing their team role. The
-- activity gets a third actor, 'team_manager' (actor_id: who); SQLite
-- changes a CHECK only by making the table again.
CREATE TABLE activity_new (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- Whose account.
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  -- 'superadmin' or 'team_manager' (actor_id: who), or 'command_line'.
  actor TEXT NOT NULL CHECK (actor IN ('superadmin', 'command_line', 'team_manager')),
  actor_id BLOB REFERENCES users(id) ON DELETE SET NULL,
  action TEXT NOT NULL,
  -- JSON: what it was about (a team's or drive's name, a role, a limit).
  detail TEXT NOT NULL DEFAULT '{}'
);
INSERT INTO activity_new (id, user_id, at, actor, actor_id, action, detail)
  SELECT id, user_id, at, actor, actor_id, action, detail FROM activity;
DROP TABLE activity;
ALTER TABLE activity_new RENAME TO activity;
CREATE INDEX activity_user ON activity (user_id, at);
