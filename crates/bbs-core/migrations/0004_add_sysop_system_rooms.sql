-- Add Sysop (id=4) and System (id=5) rooms for existing databases.
-- INSERT OR IGNORE is idempotent: safe to re-run if rooms already exist.
-- min_permission_level: 100=Sysop.
-- Linked-list order: Aides ↔ Sysop ↔ System (tail).

INSERT OR IGNORE INTO rooms
    (id, name, description, read_only, min_permission_level,
     prev_neighbor, next_neighbor, created_at)
VALUES
    (4, 'Sysop', 'Sysop-only coordination and system management.',
     0, 100, 3, 5, '2000-01-01T00:00:00Z'),
    (5, 'System', 'System announcements — read-only for non-sysops.',
     1, 100, 4, NULL, '2000-01-01T00:00:00Z');

-- Stitch Aides → Sysop only when Aides currently has no next neighbor
-- (fresh install already has this correct via 0002; existing prod databases do not).
UPDATE rooms SET next_neighbor = 4 WHERE id = 3 AND next_neighbor IS NULL;

-- Advance the autoincrement counter so user-created rooms start at 6.
INSERT OR IGNORE INTO sqlite_sequence (name, seq) VALUES ('rooms', 5);
UPDATE sqlite_sequence SET seq = MAX(seq, 5) WHERE name = 'rooms';
