-- The WebDAV drop box (docs/M2_DESIGN.md §9, question 1): an item dropped
-- through it remembers the folder it came in by. `review` always asks (the
-- item stays in the drop box until confirmed), `auto` accepts at once; ''
-- (its top) follows the user's pending mode. NULL: not dropped over WebDAV.
ALTER TABLE inbox_items ADD COLUMN dav_folder TEXT CHECK (dav_folder IN ('', 'review', 'auto'));
