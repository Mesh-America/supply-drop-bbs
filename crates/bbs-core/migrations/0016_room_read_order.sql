-- Per-room read order for `N` (read new) on public rooms (#420).
--
-- 'oldest_first' is the long-standing behaviour (unread messages are shown
-- oldest to newest), so every existing room keeps it. A sysop can switch a
-- room (an emergency channel, say) to 'newest_first' so the latest, most
-- timely message is shown first. The Mail room ignores this setting.
--
-- Append-only: never edit this file once applied (sqlx records its checksum).
ALTER TABLE rooms ADD COLUMN read_order TEXT NOT NULL DEFAULT 'oldest_first'
    CHECK (read_order IN ('oldest_first', 'newest_first'));
