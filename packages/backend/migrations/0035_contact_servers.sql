-- What a contact's server is (docs/M7E_DESIGN.md §10): the `provider` its
-- OCM discovery names (Nextcloud, ownCloud …; NULL when it names none),
-- and whether it is an Organizer server (it advertises our extensions).
-- Kept when the contact is made, refreshed whenever that server's
-- discovery is fetched again.
ALTER TABLE contacts ADD COLUMN provider TEXT;
ALTER TABLE contacts ADD COLUMN organizer INTEGER NOT NULL DEFAULT 0;
