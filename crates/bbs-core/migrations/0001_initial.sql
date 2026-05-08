-- Supply Drop BBS -- initial schema
-- Append-only: never edit a migration that has been merged to main.
-- All timestamps stored as TEXT in RFC 3339 with Z suffix.
-- All IDs are INTEGER PRIMARY KEY (SQLite ROWID alias -> i64).
-- foreign_keys = ON is enforced by the PRAGMA hook in Database::open.

CREATE TABLE IF NOT EXISTS users (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    username         TEXT    NOT NULL UNIQUE,
    display_name     TEXT,
    status           INTEGER NOT NULL DEFAULT 0
                     CHECK (status IN (0, 1, 2)),
    permission_level INTEGER NOT NULL DEFAULT 0
                     CHECK (permission_level IN (0, 10, 50, 100)),
    created_at       TEXT    NOT NULL,
    last_login_at    TEXT
);

-- Credentials live in a separate table so that no UserStore query
-- can accidentally return a password hash. Only CredentialStore
-- reads this table; it is not exposed through Host.
CREATE TABLE IF NOT EXISTS user_credentials (
    user_id    INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    phc_hash   TEXT    NOT NULL,
    updated_at TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS rooms (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    name                 TEXT    NOT NULL UNIQUE,
    description          TEXT,
    read_only            INTEGER NOT NULL DEFAULT 0
                         CHECK (read_only IN (0, 1)),
    min_permission_level INTEGER NOT NULL DEFAULT 0
                         CHECK (min_permission_level IN (0, 10, 50, 100)),
    prev_neighbor        INTEGER REFERENCES rooms(id) ON DELETE SET NULL,
    next_neighbor        INTEGER REFERENCES rooms(id) ON DELETE SET NULL,
    created_at           TEXT    NOT NULL
);

-- sender/recipient are TEXT (username strings), NOT FKs to users:
-- messages must survive user soft-deletion.
CREATE TABLE IF NOT EXISTS messages (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    sender    TEXT    NOT NULL,
    recipient TEXT,
    content   TEXT    NOT NULL,
    timestamp TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS room_messages (
    room_id    INTEGER NOT NULL REFERENCES rooms(id)    ON DELETE CASCADE,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    PRIMARY KEY (room_id, message_id)
);

-- last_read_message_id goes NULL on message delete so the read
-- pointer resets gracefully rather than breaking integrity.
CREATE TABLE IF NOT EXISTS user_room_state (
    user_id              INTEGER NOT NULL REFERENCES users(id)    ON DELETE CASCADE,
    room_id              INTEGER NOT NULL REFERENCES rooms(id)    ON DELETE CASCADE,
    last_read_message_id INTEGER          REFERENCES messages(id) ON DELETE SET NULL,
    PRIMARY KEY (user_id, room_id)
);

CREATE TABLE IF NOT EXISTS audit_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    actor        TEXT    NOT NULL,
    action       TEXT    NOT NULL,
    entity_type  TEXT    NOT NULL,
    entity_id    TEXT    NOT NULL,
    before_state TEXT,
    after_state  TEXT,
    occurred_at  TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_sender
    ON messages(sender);

CREATE INDEX IF NOT EXISTS idx_messages_recipient
    ON messages(recipient) WHERE recipient IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_room_messages_room_msg
    ON room_messages(room_id, message_id);

CREATE INDEX IF NOT EXISTS idx_user_room_state_user
    ON user_room_state(user_id);

CREATE INDEX IF NOT EXISTS idx_audit_log_occurred_at
    ON audit_log(occurred_at);
