-- A file dropped as part of a folder that was split into its files keeps
-- its path inside that folder (`Trip/2025/ticket.pdf`), for the
-- sensitive-file rules on paths.
ALTER TABLE inbox_items ADD COLUMN source_path TEXT;
