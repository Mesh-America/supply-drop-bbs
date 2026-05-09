-- Performance indexes for common query patterns.
-- room_messages(message_id): join from messages → room_messages in is_in_room().
-- user_room_state(room_id): unread_count() filters by room_id on the left side of a join.

CREATE INDEX IF NOT EXISTS idx_room_messages_message_id
    ON room_messages (message_id);

CREATE INDEX IF NOT EXISTS idx_user_room_state_room_id
    ON user_room_state (room_id);
