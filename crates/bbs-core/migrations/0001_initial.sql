-- Migration 0001: initial schema
--
-- Naming conventions:
--   * All PKs: INTEGER PRIMARY KEY (i64, rowid alias)
--   * All timestamps: TEXT NOT NULL  (RFC 3339, Z suffix)
--   * All booleans: INTEGER NOT NULL CHECK(col IN (0, 1))
--   * All enum discriminants: INTEGER NOT NULL with CHECK
--   * All FK columns: explicit ON DELETE clause — never NO ACTION
--
-- PermissionLevel discriminants (from bbs-plugin-api::PermissionLevel repr(u8)):
--   0  = Unvalidated
--   10 = User
--   50 = Aide
--   100 = Sysop
--
-- UserStatus discriminants (from bbs-core::UserStatus repr(u8)):
--   0 = Active
--   1 = Banned
--   2 = Deleted

-- ─── users ────────────────────────────────────────────────────────
--
-- Core account record. Mirrors bbs-core::User exactly.
-- Does NOT contain password material — that lives in user_credentials.

CREATE TABLE users (
    id                INTEGER PRIMARY KEY,
    username          TEXT    NOT NULL UNIQUE,
    display_name      TEXT,                       -- NULL means "show username"
    status            INTEGER NOT NULL
                        DEFAULT 0
                        CHECK (status IN (0, 1, 2)),
    permission_level  INTEGER NOT NULL
                        DEFAULT 0
                        CHECK (permission_level IN (0, 10, 50, 100)),
    created_at        TEXT    NOT NULL,
    last_login_at     TEXT                        -- NULL until first login
);

CREATE INDEX idx_users_username         ON users (username);
CREATE INDEX idx_users_permission_level ON users (permission_level);
CREATE INDEX idx_users_status           ON users (status);

-- ─── user_credentials ─────────────────────────────────────────────
--
-- Password hashes, kept separate so no UserStore query can
-- accidentally return credential material. One row per user.
-- The phc_hash column stores a PHC-format argon2id string:
--   $argon2id$v=19$m=<mem>,t=<iter>,p=<par>$<b64-salt>$<b64-hash>
-- The salt is embedded in the PHC string; no separate salt column.

CREATE TABLE user_credentials (
    user_id     INTEGER PRIMARY KEY
                  REFERENCES users (id) ON DELETE CASCADE,
    phc_hash    TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL       -- when the hash was last written
);

-- ─── rooms ────────────────────────────────────────────────────────
--
-- Mirrors bbs-core::Room. The linked-list invariant (exactly one
-- head, exactly one tail, no cycles) is enforced by
-- verify_room_walk_order() at startup and maintained by the
-- RoomStore::reorder transaction.

CREATE TABLE rooms (
    id                   INTEGER PRIMARY KEY,
    name                 TEXT    NOT NULL UNIQUE,
    description          TEXT,                   -- NULL means no description
    read_only            INTEGER NOT NULL
                           DEFAULT 0
                           CHECK (read_only IN (0, 1)),
    min_permission_level INTEGER NOT NULL
                           DEFAULT 10
                           CHECK (min_permission_level IN (0, 10, 50, 100)),
    prev_neighbor        INTEGER
                           REFERENCES rooms (id) ON DELETE SET NULL,
    next_neighbor        INTEGER
                           REFERENCES rooms (id) ON DELETE SET NULL,
    created_at           TEXT    NOT NULL
);

CREATE INDEX idx_rooms_name ON rooms (name);

-- ─── messages ─────────────────────────────────────────────────────
--
-- Mirrors bbs-core::Message. Append-only in normal operation;
-- sysops may delete rows (with audit trail). The sender and
-- recipient columns reference users.username (TEXT), not users.id,
-- because:
--   (a) messages carry Username (validated string), not UserId, in
--       the domain model;
--   (b) a deleted user's messages are retained (soft-delete policy
--       in UserStatus::Deleted); ON DELETE RESTRICT means a deleted
--       user's messages must be reassigned or removed first, which
--       is the correct sysop workflow.
--
-- DMs: recipient IS NOT NULL.
-- Room posts: recipient IS NULL; association is in room_messages.

CREATE TABLE messages (
    id          INTEGER PRIMARY KEY,
    sender      TEXT    NOT NULL
                  REFERENCES users (username) ON DELETE RESTRICT,
    recipient   TEXT
                  REFERENCES users (username) ON DELETE RESTRICT,
    content     TEXT    NOT NULL,
    timestamp   TEXT    NOT NULL
);

CREATE INDEX idx_messages_sender    ON messages (sender);
CREATE INDEX idx_messages_recipient ON messages (recipient)
    WHERE recipient IS NOT NULL;
CREATE INDEX idx_messages_timestamp ON messages (timestamp);

-- ─── room_messages ────────────────────────────────────────────────
--
-- Join table linking public posts to rooms. A message is in at most
-- one room. DMs do not appear here.
--
-- ON DELETE CASCADE from rooms: if a room is deleted, its message
-- associations are removed. The messages themselves survive (sysop
-- may want to reassign them). This is intentional.
--
-- ON DELETE CASCADE from messages: if a message is deleted (sysop
-- action), its room association is removed automatically.

CREATE TABLE room_messages (
    room_id     INTEGER NOT NULL
                  REFERENCES rooms (id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL
                  REFERENCES messages (id) ON DELETE CASCADE,
    PRIMARY KEY (room_id, message_id)
);

CREATE INDEX idx_room_messages_room_id    ON room_messages (room_id, message_id);
CREATE INDEX idx_room_messages_message_id ON room_messages (message_id);

-- ─── user_room_state ──────────────────────────────────────────────
--
-- Per-user per-room "last seen" pointer. Drives the unread-message
-- count and the "next unread" walk.
--
-- ON DELETE CASCADE from users: when a user is hard-deleted, their
-- read-state rows vanish.
--
-- ON DELETE CASCADE from rooms: if a room is deleted, all users'
-- read-state for that room vanishes.
--
-- last_read_message_id: SET NULL when the pointed-to message is
-- deleted. A NULL here means "show everything from the beginning"
-- — the conservative choice, not "show nothing."

CREATE TABLE user_room_state (
    user_id              INTEGER NOT NULL
                           REFERENCES users (id) ON DELETE CASCADE,
    room_id              INTEGER NOT NULL
                           REFERENCES rooms (id) ON DELETE CASCADE,
    last_read_message_id INTEGER
                           REFERENCES messages (id) ON DELETE SET NULL,
    updated_at           TEXT    NOT NULL,
    PRIMARY KEY (user_id, room_id)
);

CREATE INDEX idx_user_room_state_user ON user_room_state (user_id);

-- ─── sessions ─────────────────────────────────────────────────────
--
-- Live and recently-expired sessions. session_token stores a
-- SHA-256 hex digest of the raw 256-bit token; never the raw token.
-- A NULL user_id means the session is pre-authentication.

CREATE TABLE sessions (
    id              INTEGER PRIMARY KEY,
    session_token   TEXT    NOT NULL UNIQUE,  -- SHA-256(raw token), hex
    user_id         INTEGER
                      REFERENCES users (id) ON DELETE CASCADE,
    transport       TEXT    NOT NULL,
    created_at      TEXT    NOT NULL,
    last_active_at  TEXT    NOT NULL,
    expires_at      TEXT    NOT NULL
);

CREATE INDEX idx_sessions_token      ON sessions (session_token);
CREATE INDEX idx_sessions_user_id    ON sessions (user_id)
    WHERE user_id IS NOT NULL;
CREATE INDEX idx_sessions_expires_at ON sessions (expires_at);

-- ─── workflows ────────────────────────────────────────────────────
--
-- Persisted state machine state for multi-step flows (registration,
-- login challenge, sysop-mediated validation). One active workflow
-- per session at most (UNIQUE on session_id).
--
-- workflow_kind: TEXT discriminant (e.g., "registration", "login").
-- step: TEXT name of the current state-machine step.
-- state_json: JSON blob encoding the full state for the current step.

CREATE TABLE workflows (
    id            INTEGER PRIMARY KEY,
    session_id    INTEGER NOT NULL UNIQUE
                    REFERENCES sessions (id) ON DELETE CASCADE,
    workflow_kind TEXT    NOT NULL,
    step          TEXT    NOT NULL,
    state_json    TEXT    NOT NULL,
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL
);

CREATE INDEX idx_workflows_session ON workflows (session_id);

-- ─── login_failures ───────────────────────────────────────────────
--
-- Rate limiting + security report input. One row per failed login
-- attempt. remote_id is transport-specific (MeshCore public-key
-- hex, IP address for web, etc.) — stored as TEXT.

CREATE TABLE login_failures (
    id                 INTEGER PRIMARY KEY,
    transport          TEXT    NOT NULL,
    attempted_username TEXT    NOT NULL,
    remote_id          TEXT,            -- NULL if transport can't identify peer
    occurred_at        TEXT    NOT NULL
);

CREATE INDEX idx_login_failures_transport_time
    ON login_failures (transport, occurred_at);
CREATE INDEX idx_login_failures_username
    ON login_failures (attempted_username);

-- ─── audit_log ────────────────────────────────────────────────────
--
-- Append-only record of sysop actions. No row is ever deleted via
-- the application layer (there is no delete path in AuditStore).
--
-- actor: the username of the sysop/aide who performed the action.
-- action: a short machine-readable code (e.g., "user.delete",
--   "room.create", "message.delete", "user.permission_change").
-- target_kind / target_id: what was acted upon ("user" / "42").
-- before_json / after_json: domain-object snapshots taken immediately
--   before and after the action. NULL where not meaningful.
-- session_id: the sessions.id at time of action (for correlation).

CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY,
    occurred_at TEXT    NOT NULL,
    actor       TEXT    NOT NULL,     -- username
    session_id  INTEGER NOT NULL,     -- sessions.id at time of action
    action      TEXT    NOT NULL,
    target_kind TEXT    NOT NULL,
    target_id   TEXT    NOT NULL,
    before_json TEXT,
    after_json  TEXT
);

CREATE INDEX idx_audit_log_occurred_at ON audit_log (occurred_at);
CREATE INDEX idx_audit_log_actor       ON audit_log (actor);
CREATE INDEX idx_audit_log_action      ON audit_log (action);
