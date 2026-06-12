-- Replace 6-byte pubkey_prefix primary key with the full 32-byte pubkey (SYN-11).
--
-- The previous schema stored only the first 6 bytes of each node's Ed25519
-- public key as the credential identifier.  A 48-bit identifier space is
-- small enough that two nodes in the same mesh could share a prefix
-- (birthday bound ~1/(2^48)); when they do, one node's credential slot
-- silently overwrites the other's via ON CONFLICT DO UPDATE.
--
-- Existing rows cannot be migrated because the full 32-byte pubkeys are not
-- stored anywhere — only the 6-byte prefixes.  All stored bindings are
-- therefore dropped.  Users will need to log in once after this upgrade to
-- re-establish their auto-login credential.

DROP TABLE IF EXISTS node_credentials;

CREATE TABLE node_credentials (
    pubkey    BLOB    NOT NULL PRIMARY KEY,  -- full 32-byte Ed25519 public key
    user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_auth TEXT    NOT NULL
);
