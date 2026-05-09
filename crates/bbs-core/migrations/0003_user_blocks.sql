-- User block list: lets a user hide another user's messages from their view.
-- blocker/blocked are TEXT usernames (not FKs) so blocks survive account deletion.
CREATE TABLE IF NOT EXISTS user_blocks (
    blocker  TEXT NOT NULL,
    blocked  TEXT NOT NULL,
    PRIMARY KEY (blocker, blocked)
);

CREATE INDEX IF NOT EXISTS idx_user_blocks_blocker ON user_blocks(blocker);
