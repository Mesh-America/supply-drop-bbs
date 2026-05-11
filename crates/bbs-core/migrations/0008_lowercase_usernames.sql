-- Normalise all existing usernames to lower-case ASCII.
--
-- Username::new() was updated to enforce lower-case at registration time, but
-- accounts created before that change may still have mixed-case usernames in
-- the database.  All lookup paths compare against the lower-cased form that
-- Username::new() now produces, so mixed-case rows are invisible to the BBS.
--
-- Collision guard: if two accounts somehow have usernames that are identical
-- after lowercasing (e.g. "Alice" and "alice"), we skip normalising the
-- mixed-case row rather than violating the UNIQUE constraint.  The operator
-- would need to resolve such duplicates manually; in practice this cannot
-- happen because SQLite's case-sensitive UNIQUE already prevented two users
-- from registering names that differ only in case.
--
-- node_credentials stores user_id (integer FK), not a username text column,
-- so no update is needed there.

UPDATE users
   SET username = lower(username)
 WHERE username != lower(username)
   AND NOT EXISTS (
       SELECT 1 FROM users u2
        WHERE lower(u2.username) = lower(users.username)
          AND u2.id != users.id
   );

-- user_blocks stores plain TEXT usernames (not FKs) and has a composite PK.
-- Update each column separately and skip rows that would produce a PK conflict.

UPDATE user_blocks
   SET blocker = lower(blocker)
 WHERE blocker != lower(blocker)
   AND NOT EXISTS (
       SELECT 1 FROM user_blocks ub2
        WHERE ub2.blocker = lower(user_blocks.blocker)
          AND ub2.blocked = user_blocks.blocked
   );

UPDATE user_blocks
   SET blocked = lower(blocked)
 WHERE blocked != lower(blocked)
   AND NOT EXISTS (
       SELECT 1 FROM user_blocks ub2
        WHERE ub2.blocker = user_blocks.blocker
          AND ub2.blocked = lower(user_blocks.blocked)
   );

-- messages stores sender/recipient as plain TEXT with no uniqueness constraint,
-- so a simple bulk update is safe.

UPDATE messages
   SET sender = lower(sender)
 WHERE sender != lower(sender);

UPDATE messages
   SET recipient = lower(recipient)
 WHERE recipient IS NOT NULL
   AND recipient != lower(recipient);

-- audit_log: normalise BBS-username actors (skip 'system' and 'web:*' prefixes).
UPDATE audit_log
   SET actor = lower(actor)
 WHERE actor != lower(actor)
   AND actor != 'system'
   AND actor NOT LIKE 'web:%';
