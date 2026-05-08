//! Integration tests for `MessageStore` and `RoomStore`.

use bbs_core::{Database, MessageStore, RoomStore, Timestamp, UserStore};
use bbs_plugin_api::{PermissionLevel, Username};

async fn test_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sqlite");
    let db = Database::open(path.to_str().unwrap()).await.unwrap();
    (db, dir)
}

async fn seed_user(db: &Database, name: &str) -> Username {
    let u = Username::new(name).unwrap();
    UserStore::create(db, &u, None, PermissionLevel::User, Timestamp::now())
        .await
        .unwrap();
    u
}

#[tokio::test]
async fn post_messages_and_paginate() {
    let (db, _dir) = test_db().await;
    let alice = seed_user(&db, "alice").await;

    let room_id = RoomStore::create(
        &db,
        "lobby",
        None,
        false,
        PermissionLevel::User,
        Timestamp::now(),
    )
    .await
    .unwrap();

    let mut expected_ids = Vec::new();
    for i in 0u8..5 {
        let mid = db
            .post_to_room(room_id, &alice, &format!("msg {i}"), Timestamp::now())
            .await
            .unwrap();
        expected_ids.push(mid);
    }

    // First page: 3 messages.
    let page1 = db.list_in_room(room_id, None, 3).await.unwrap();
    assert_eq!(page1.messages.len(), 3);
    assert!(page1.next_cursor.is_some(), "expected a cursor for page 2");

    // Second page: 2 remaining messages.
    let page2 = db
        .list_in_room(room_id, page1.next_cursor, 3)
        .await
        .unwrap();
    assert_eq!(page2.messages.len(), 2);
    assert!(page2.next_cursor.is_none(), "expected last page");

    let all_ids: Vec<_> = page1
        .messages
        .iter()
        .chain(&page2.messages)
        .map(|m| m.id)
        .collect();
    assert_eq!(
        all_ids, expected_ids,
        "pagination must return all IDs in order"
    );
}

#[tokio::test]
async fn direct_messages_only_appear_in_list_direct() {
    let (db, _dir) = test_db().await;
    let alice = seed_user(&db, "alice").await;
    let bob = seed_user(&db, "bob").await;

    let dm_id = db
        .post_direct(&alice, &bob, "hey bob", Timestamp::now())
        .await
        .unwrap();

    // DM appears in list_direct for both parties.
    let alice_dms = db.list_direct(&alice, None, 10).await.unwrap();
    assert_eq!(alice_dms.messages.len(), 1);
    assert_eq!(alice_dms.messages[0].id, dm_id);

    let bob_dms = db.list_direct(&bob, None, 10).await.unwrap();
    assert_eq!(bob_dms.messages.len(), 1);
    assert_eq!(bob_dms.messages[0].id, dm_id);
}

#[tokio::test]
async fn unread_count_and_mark_read() {
    let (db, _dir) = test_db().await;
    let alice = seed_user(&db, "alice").await;
    let alice_id = db.get_by_username(&alice).await.unwrap().unwrap().id;

    let room_id = RoomStore::create(
        &db,
        "news",
        None,
        false,
        PermissionLevel::User,
        Timestamp::now(),
    )
    .await
    .unwrap();

    // 3 messages posted.
    let mut ids = Vec::new();
    for i in 0u8..3 {
        ids.push(
            db.post_to_room(room_id, &alice, &format!("post {i}"), Timestamp::now())
                .await
                .unwrap(),
        );
    }

    // All 3 are unread initially.
    let unread = db.unread_count(alice_id, room_id).await.unwrap();
    assert_eq!(unread, 3);

    // Mark the first message read.
    db.mark_read(alice_id, room_id, ids[0]).await.unwrap();
    let unread = db.unread_count(alice_id, room_id).await.unwrap();
    assert_eq!(unread, 2);

    // Mark all read (up to last message).
    db.mark_read(alice_id, room_id, ids[2]).await.unwrap();
    let unread = db.unread_count(alice_id, room_id).await.unwrap();
    assert_eq!(unread, 0);

    // Calling mark_read again with an older id must not advance backward.
    db.mark_read(alice_id, room_id, ids[0]).await.unwrap();
    let unread = db.unread_count(alice_id, room_id).await.unwrap();
    assert_eq!(unread, 0);
}

#[tokio::test]
async fn delete_message_sets_read_pointer_null() {
    let (db, _dir) = test_db().await;
    let alice = seed_user(&db, "alice").await;
    let alice_id = db.get_by_username(&alice).await.unwrap().unwrap().id;

    let room_id = RoomStore::create(
        &db,
        "general",
        None,
        false,
        PermissionLevel::User,
        Timestamp::now(),
    )
    .await
    .unwrap();

    let mid = db
        .post_to_room(room_id, &alice, "ephemeral", Timestamp::now())
        .await
        .unwrap();

    db.mark_read(alice_id, room_id, mid).await.unwrap();
    assert_eq!(db.unread_count(alice_id, room_id).await.unwrap(), 0);

    // Deleting the message should set the read pointer to NULL,
    // which means the count stays 0 (no messages to count).
    let deleted = MessageStore::delete(&db, mid).await.unwrap();
    assert!(deleted);

    assert_eq!(db.unread_count(alice_id, room_id).await.unwrap(), 0);
}

// ── Proptest: message content round-trip ─────────────────────────────

use proptest::prelude::*;

proptest! {
    #[test]
    fn message_content_roundtrip(
        content in "[^\x00]{1,512}"
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("prop.sqlite");
            let db = Database::open(path.to_str().unwrap()).await.unwrap();

            let alice = Username::new("alice").unwrap();
            UserStore::create(&db, &alice, None, PermissionLevel::User, Timestamp::now())
                .await
                .unwrap();

            let room_id = RoomStore::create(&db, "room", None, false, PermissionLevel::User, Timestamp::now())
                .await
                .unwrap();

            let mid = db
                .post_to_room(room_id, &alice, &content, Timestamp::now())
                .await
                .unwrap();

            let fetched = MessageStore::get_by_id(&db, mid)
                .await
                .unwrap()
                .expect("just posted — must be present");

            assert_eq!(fetched.content, content);
        });
    }
}
