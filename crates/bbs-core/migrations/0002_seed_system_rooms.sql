-- Seed the five built-in system rooms.
-- INSERT OR IGNORE is idempotent: safe to re-run on existing databases.
-- IDs are stable constants (LOBBY=1, MAIL=2, AIDES=3, SYSOP=4, SYSTEM=5) referenced in bbs-core.
-- Linked-list order: Lobby ↔ Mail ↔ Aides ↔ Sysop ↔ System (head → tail).
-- min_permission_level: 0=Unvalidated, 10=User, 50=Aide, 100=Sysop.

INSERT OR IGNORE INTO rooms
    (id, name, description, read_only, min_permission_level,
     prev_neighbor, next_neighbor, created_at)
VALUES
    (1, 'Lobby', 'Public gathering place — say hello!',
     0, 0, NULL, 2, '2000-01-01T00:00:00Z'),
    (2, 'Mail', 'Private messages — type E to compose, enter recipient when prompted.',
     0, 10, 1, 3, '2000-01-01T00:00:00Z'),
    (3, 'Aides', 'Aide coordination room.',
     0, 50, 2, 4, '2000-01-01T00:00:00Z'),
    (4, 'Sysop', 'Sysop-only coordination and system management.',
     0, 100, 3, 5, '2000-01-01T00:00:00Z'),
    (5, 'System', 'System announcements — read-only for non-sysops.',
     1, 100, 4, NULL, '2000-01-01T00:00:00Z');

-- Keep the autoincrement counter consistent so user-created rooms start at 6.
-- sqlite_sequence is updated automatically when rows with explicit IDs are
-- inserted into an AUTOINCREMENT table, but only if the seq column exists.
-- We guard with INSERT OR IGNORE in case the sequence row already reflects
-- a higher value (upgrade scenario where rooms already exist).
INSERT OR IGNORE INTO sqlite_sequence (name, seq) VALUES ('rooms', 5);
UPDATE sqlite_sequence SET seq = MAX(seq, 5) WHERE name = 'rooms';
