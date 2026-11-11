-- Files that go with an item (a page snapshot's Markdown copy, next to its
-- HTML): staged with it, placed beside it under the same name with their
-- own extension, moved and taken back with it.
ALTER TABLE inbox_items ADD COLUMN companions TEXT NOT NULL DEFAULT '[]';
ALTER TABLE placements ADD COLUMN companion_nodes TEXT NOT NULL DEFAULT '[]';
