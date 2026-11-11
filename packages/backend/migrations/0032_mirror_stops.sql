-- docs/M7D_DESIGN.md §9, finishing M7d.
--
-- When a mirror stopped: noted the first time a look finds it stopped
-- (`Files::mirror_stop`), cleared when it goes on; NULL while it copies.
ALTER TABLE mirrors ADD COLUMN stopped_at TEXT;

-- The drive deleted (its id set to NULL by the foreign key): stopped
-- then, whatever stopped it before.
CREATE TRIGGER mirrors_drive_deleted AFTER UPDATE OF source_id ON mirrors
WHEN OLD.source_id IS NOT NULL AND NEW.source_id IS NULL
BEGIN
  UPDATE mirrors SET stopped_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = NEW.id;
END;

-- An item in a mirror's trash (or in a drive ejected from one) keeps the
-- ids of the drive mirrored that its node had (`nodes.source_id`): its
-- own, its folder's and its encrypted folder's, the ids its name and its
-- keys are sealed with.
ALTER TABLE trash ADD COLUMN source_id BLOB;
ALTER TABLE trash ADD COLUMN parent_source_id BLOB;
ALTER TABLE trash ADD COLUMN e2e_root_source_id BLOB;
