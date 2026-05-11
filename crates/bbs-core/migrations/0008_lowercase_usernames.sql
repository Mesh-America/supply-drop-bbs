-- Normalise all existing usernames to lower-case ASCII.
--
-- Username::new() was updated to enforce lower-case at registration time, but
-- accounts created before that change may still have mixed-case usernames in
-- the database.  All lookup paths compare against the lower-cased form that
-- Username::new() now produces, so mixed-case rows are invisible to the BBS.
--
-- This migration lowercases every username and all foreign-key references to
-- usernames in sibling tables (node_credentials, user_blocks, messages,
-- audit_log) so the data stays consistent.

UPDATE users
   SET username = lower(username)
 WHERE username != lower(username);

UPDATE node_credentials
   SET username = lower(username)
 WHERE username != lower(username);

UPDATE user_blocks
   SET blocker  = lower(blocker)
 WHERE blocker  != lower(blocker);

UPDATE user_blocks
   SET blocked  = lower(blocked)
 WHERE blocked  != lower(blocked);

UPDATE messages
   SET sender    = lower(sender)
 WHERE sender    != lower(sender);

UPDATE messages
   SET recipient = lower(recipient)
 WHERE recipient IS NOT NULL
   AND recipient != lower(recipient);

UPDATE audit_log
   SET actor  = lower(actor)
 WHERE actor  != lower(actor)
   AND actor  != 'system'
   AND actor  NOT LIKE 'web:%';
