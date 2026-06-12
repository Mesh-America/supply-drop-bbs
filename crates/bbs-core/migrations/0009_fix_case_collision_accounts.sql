-- Resolve case-collision accounts left inaccessible by migration 0008 (SYN-30).
--
-- Migration 0008 skipped lowercasing any `users` row whose lower-cased name
-- was already taken by a different account (e.g. "Alice" when "alice" already
-- existed).  This was described in 0008's comment as impossible because "the
-- case-sensitive UNIQUE constraint already prevented two accounts with names
-- that differ only in case from coexisting."  That claim is incorrect: a
-- case-sensitive UNIQUE index allows both "alice" AND "Alice" to coexist.
--
-- After 0008, every row where `username != lower(username)` is permanently
-- unreachable through normal BBS lookup paths — Username::new() enforces
-- lower-case, so the account is effectively orphaned.
--
-- Resolution: rename each orphaned account to `lower(username) || '_' || id`
-- (e.g. user id=42 named "Alice" when "alice" already exists becomes
-- "alice_42").  The new name is guaranteed unique because `id` is the primary
-- key.  Operators should inspect any renamed accounts (check the audit_log
-- or run `SELECT * FROM users WHERE username LIKE '%_[0-9]*'` ) and decide
-- whether to merge them with the already-lowercased counterpart or keep them.
--
-- user_blocks uses plain TEXT (no FK) and has a collision guard in 0008,
-- so it may also have orphaned mixed-case rows — update those too.
--
-- messages, audit_log: 0008 updated these unconditionally (no collision guard),
-- so they are already fully lowercase and need no changes here.

-- Step 1: Fix user_blocks before changing users (references old usernames).
UPDATE user_blocks
   SET blocker = lower(blocker) || '_' || (SELECT id FROM users WHERE username = user_blocks.blocker)
 WHERE blocker != lower(blocker)
   AND EXISTS (SELECT 1 FROM users WHERE username = user_blocks.blocker);

UPDATE user_blocks
   SET blocked = lower(blocked) || '_' || (SELECT id FROM users WHERE username = user_blocks.blocked)
 WHERE blocked != lower(blocked)
   AND EXISTS (SELECT 1 FROM users WHERE username = user_blocks.blocked);

-- Step 2: Rename the orphaned users.
UPDATE users
   SET username = lower(username) || '_' || id
 WHERE username != lower(username);
