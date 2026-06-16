-- Add a "must change password at next login" flag to stored credentials.
--
-- Set when a sysop resets an account's password to a server-generated temporary
-- value (`.PW`): the temp password is single-use and the user is forced to choose
-- a new one before their login completes. See issue #134.
--
-- Append-only: never edit this file once applied (sqlx records its checksum).
ALTER TABLE user_credentials ADD COLUMN must_change INTEGER NOT NULL DEFAULT 0;
