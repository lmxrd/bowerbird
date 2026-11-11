-- On-demand folders (docs/M5B_DESIGN.md): a placeholder shows the
-- server's size and time before anything is downloaded. Known from the
-- listings; null when the change feed told of a change (it does not say
-- them), and then asked for when a placeholder needs them.
ALTER TABLE remote ADD COLUMN size INTEGER;
ALTER TABLE remote ADD COLUMN modified TEXT;
