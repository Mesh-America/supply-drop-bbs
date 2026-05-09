-- Persistent mesh-node → user binding for auto-login.
--
-- pubkey_prefix: first 6 bytes of the MeshCore node Ed25519 public key,
--   stored as a BLOB. Uniquely identifies a radio node.
-- user_id: the BBS user this node last authenticated as.
--   ON DELETE CASCADE removes the binding when the user account is deleted.
-- last_auth: RFC 3339 timestamp of the most recent successful authentication.
--   Used to enforce the configurable credential TTL (default 14 days).

CREATE TABLE IF NOT EXISTS node_credentials (
    pubkey_prefix BLOB    NOT NULL PRIMARY KEY,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_auth     TEXT    NOT NULL
);
