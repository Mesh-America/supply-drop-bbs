-- Durable audit log for privileged sysop and aide actions.
--
-- Every destructive or elevated action (ban, unban, validate, delete message,
-- create/delete room, permission changes) is written here so there is a
-- permanent, queryable record of who did what and when.
--
-- actor   : BBS username that performed the action, or "system" for
--           host-initiated events, or "web:<username>" for actions taken
--           through the admin web UI by a sysop who may not have a BBS session.
-- action  : Short label — one of: ban, unban, validate, delete_message,
--           create_room, delete_room, set_permission.
-- target  : What was acted on (username, "#<message_id>", room name).
-- detail  : Optional free-form context (e.g. "level 10 -> 100").

-- 0001_initial.sql created a skeleton audit_log table with a different schema
-- (used 'occurred_at' instead of 'created_at', no actor/action/target/detail).
-- That table was never populated.  Drop it so we can create the real schema.
DROP TABLE IF EXISTS audit_log;

CREATE TABLE IF NOT EXISTS audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    actor      TEXT    NOT NULL,
    action     TEXT    NOT NULL,
    target     TEXT,
    detail     TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_audit_log_created_at ON audit_log (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_actor       ON audit_log (actor);
CREATE INDEX IF NOT EXISTS idx_audit_log_action      ON audit_log (action);
