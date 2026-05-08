//! Integration tests for `UserStore`. Each test gets its own
//! `tempfile::tempdir()` so tests are fully isolated.

use bbs_core::{Database, StoreError, Timestamp, UserStore};
use bbs_plugin_api::{PermissionLevel, Username};

async fn test_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sqlite");
    let db = Database::open(path.to_str().unwrap()).await.unwrap();
    // Return dir so it stays alive for the duration of the test.
    (db, dir)
}

#[tokio::test]
async fn insert_user_and_fetch_by_username() {
    let (db, _dir) = test_db().await;
    let username = Username::new("alice").unwrap();

    let id = db
        .create(
            &username,
            Some("Alice"),
            PermissionLevel::User,
            Timestamp::now(),
        )
        .await
        .expect("create should succeed");

    let fetched = db
        .get_by_username(&username)
        .await
        .unwrap()
        .expect("should find user");

    assert_eq!(fetched.id, id);
    assert_eq!(fetched.username, username);
    assert_eq!(fetched.display_name.as_deref(), Some("Alice"));
    assert!(fetched.last_login_at.is_none());
}

#[tokio::test]
async fn duplicate_username_returns_conflict() {
    let (db, _dir) = test_db().await;
    let username = Username::new("alice").unwrap();

    db.create(&username, None, PermissionLevel::User, Timestamp::now())
        .await
        .unwrap();

    let err = db
        .create(&username, None, PermissionLevel::User, Timestamp::now())
        .await
        .unwrap_err();

    assert!(
        matches!(err, StoreError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );
}

#[tokio::test]
async fn get_by_id_returns_none_for_missing() {
    let (db, _dir) = test_db().await;
    use bbs_core::UserId;
    let result = db.get_by_id(UserId::new(9999)).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn update_display_name_and_status() {
    let (db, _dir) = test_db().await;
    use bbs_core::user::UserStatus;

    let username = Username::new("bob").unwrap();
    let id = db
        .create(&username, None, PermissionLevel::User, Timestamp::now())
        .await
        .unwrap();

    db.update(
        id,
        Some(Some("Bobby")),
        Some(UserStatus::Banned),
        None,
        None,
    )
    .await
    .unwrap();

    let updated = db.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(updated.display_name.as_deref(), Some("Bobby"));
    assert_eq!(updated.status, UserStatus::Banned);
}

#[tokio::test]
async fn hard_delete_fails_when_messages_exist() {
    let (db, _dir) = test_db().await;

    let alice = Username::new("alice").unwrap();
    let alice_id = db
        .create(&alice, None, PermissionLevel::User, Timestamp::now())
        .await
        .unwrap();

    // Create a room and post a message so hard_delete should be refused.
    use bbs_core::{MessageStore, RoomStore};
    let room_id = db
        .create("lobby", None, false, PermissionLevel::User, Timestamp::now())
        .await
        .unwrap();

    db.post_to_room(room_id, &alice, "hello", Timestamp::now())
        .await
        .unwrap();

    let err = db.hard_delete(alice_id).await.unwrap_err();
    assert!(
        matches!(err, StoreError::IntegrityViolation(_)),
        "expected IntegrityViolation, got {err:?}"
    );
}

#[tokio::test]
async fn list_filters_by_status() {
    let (db, _dir) = test_db().await;
    use bbs_core::user::UserStatus;

    for name in ["alice", "bob", "carol"] {
        let u = Username::new(name).unwrap();
        db.create(&u, None, PermissionLevel::User, Timestamp::now())
            .await
            .unwrap();
    }

    // Ban bob.
    let bob = db
        .get_by_username(&Username::new("bob").unwrap())
        .await
        .unwrap()
        .unwrap();
    db.update(bob.id, None, Some(UserStatus::Banned), None, None)
        .await
        .unwrap();

    let active = db
        .list(Some(UserStatus::Active), 100, 0)
        .await
        .unwrap();
    assert_eq!(active.len(), 2);

    let banned = db
        .list(Some(UserStatus::Banned), 100, 0)
        .await
        .unwrap();
    assert_eq!(banned.len(), 1);
    assert_eq!(banned[0].username.as_str(), "bob");
}

// ── Proptest: username round-trip ─────────────────────────────────────

use proptest::prelude::*;

proptest! {
    #[test]
    fn username_roundtrip_create_fetch(
        raw in "[a-z][a-z0-9]{0,30}"
    ) {
        // Filter to names that pass Username::new validation.
        let username = match Username::new(raw.clone()) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("prop.sqlite");
            let db = Database::open(path.to_str().unwrap()).await.unwrap();

            let id = db
                .create(&username, None, PermissionLevel::User, Timestamp::now())
                .await
                .unwrap();

            let fetched = db
                .get_by_username(&username)
                .await
                .unwrap()
                .expect("just inserted — must be present");

            assert_eq!(fetched.id, id);
            assert_eq!(fetched.username.as_str(), raw.as_str());
        });
    }
}
