-- Roles (docs/M7B_DESIGN.md §7). Per node of the mirror:
--
-- - `role`: what the user may do with it (`my_role`: upload_only, viewer,
--   editor, manager, owner), as the listings say it; a node the change
--   feed told of keeps its own where it stays, else takes its parent's.
--   Null until listed: a mirror of before roles is listed again at the
--   next pass, its top's role being new to it.
-- - `drive_id`: the drive it is in, whose trash says whether something
--   gone from the feed was deleted or is only no longer seen.
ALTER TABLE remote ADD COLUMN role TEXT;
ALTER TABLE remote ADD COLUMN drive_id TEXT;
