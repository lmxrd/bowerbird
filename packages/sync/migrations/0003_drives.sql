-- Drives (docs/M7A_DESIGN.md): the drive a synced folder is in (a string,
-- as the API gives it), for the settings' list of drives. Null until the
-- engine has asked the server (a folder added before drives: at its next
-- pass).
ALTER TABLE folders ADD COLUMN drive_id TEXT;
