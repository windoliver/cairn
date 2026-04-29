//! Session storage round-trip (brief §8.1).
//!
//! Pins the inherent session methods on `SqliteMemoryStore`: discovery,
//! creation, idle-window reuse semantics through the pure resolver,
//! touch / end lifecycle, and `(user, agent, project_root)` isolation.

#![allow(missing_docs)]

use cairn_core::domain::Identity;
use cairn_core::domain::session::{
    DEFAULT_IDLE_WINDOW_SECS, SessionDecision, SessionIdentity, resolve_session,
};
use cairn_store_sqlite::{NewSessionMetadata, open_in_memory};

fn user() -> Identity {
    Identity::parse("usr:alice").expect("valid")
}

fn agent() -> Identity {
    Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid")
}

fn identity(project: Option<&str>) -> SessionIdentity {
    SessionIdentity::new(user(), agent(), project.map(str::to_owned)).expect("valid")
}

#[tokio::test]
async fn first_call_finds_no_active_session() {
    let store = open_in_memory().await.expect("open");
    let got = store
        .find_active_session(&identity(Some("/repo")))
        .await
        .expect("find");
    assert!(got.is_none());
}

#[tokio::test]
async fn create_then_find_returns_same_id() {
    let store = open_in_memory().await.expect("open");
    let id_a = identity(Some("/repo"));
    let session = store
        .create_session(&id_a, NewSessionMetadata::default())
        .await
        .expect("create");

    let found = store
        .find_active_session(&id_a)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id, session.id);
    // Idle is well within the 24 h window for a freshly-minted row.
    assert!(found.idle_secs < DEFAULT_IDLE_WINDOW_SECS);
}

#[tokio::test]
async fn resolver_reuses_recent_session() {
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let session = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("create");
    let last = store
        .find_active_session(&id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(
        resolve_session(Some(last), DEFAULT_IDLE_WINDOW_SECS),
        SessionDecision::Reuse(session.id),
    );
}

#[tokio::test]
async fn different_project_root_isolates_sessions() {
    let store = open_in_memory().await.expect("open");
    let s_a = store
        .create_session(&identity(Some("/repo-a")), NewSessionMetadata::default())
        .await
        .expect("create a");
    let s_b = store
        .create_session(&identity(Some("/repo-b")), NewSessionMetadata::default())
        .await
        .expect("create b");
    assert_ne!(s_a.id, s_b.id);

    let found_a = store
        .find_active_session(&identity(Some("/repo-a")))
        .await
        .expect("find a")
        .expect("present");
    assert_eq!(found_a.id, s_a.id);
}

#[tokio::test]
async fn null_and_set_project_root_are_distinct() {
    let store = open_in_memory().await.expect("open");
    let with_root = store
        .create_session(&identity(Some("/repo")), NewSessionMetadata::default())
        .await
        .expect("create with");
    let without_root = store
        .create_session(&identity(None), NewSessionMetadata::default())
        .await
        .expect("create without");
    assert_ne!(with_root.id, without_root.id);

    let found_none = store
        .find_active_session(&identity(None))
        .await
        .expect("find none")
        .expect("present");
    assert_eq!(found_none.id, without_root.id);
}

#[tokio::test]
async fn touch_advances_last_activity() {
    let store = open_in_memory().await.expect("open");
    let session = store
        .create_session(&identity(Some("/repo")), NewSessionMetadata::default())
        .await
        .expect("create");
    let before = store
        .get_session(&session.id)
        .await
        .expect("get")
        .expect("present");

    // SQLite millisecond timestamps may collide if invoked back-to-back; sleep
    // a tick to make the assertion non-flaky.
    std::thread::sleep(std::time::Duration::from_millis(5));

    assert!(
        store.touch_session(&session.id).await.expect("touch"),
        "active session should bump",
    );
    let after = store
        .get_session(&session.id)
        .await
        .expect("get")
        .expect("present");
    assert!(after.last_activity_at_unix_ms >= before.last_activity_at_unix_ms);
}

#[tokio::test]
async fn end_session_excludes_from_discovery() {
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let session = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("create");
    assert!(store.end_session(&session.id).await.expect("end"));
    let found = store.find_active_session(&id).await.expect("find");
    assert!(found.is_none(), "ended session must not be returned");
    // Idempotent re-end is a no-op.
    assert!(!store.end_session(&session.id).await.expect("re-end"));
}

#[tokio::test]
async fn touch_on_ended_session_is_noop() {
    let store = open_in_memory().await.expect("open");
    let session = store
        .create_session(&identity(Some("/repo")), NewSessionMetadata::default())
        .await
        .expect("create");
    assert!(store.end_session(&session.id).await.expect("end"));
    assert!(!store.touch_session(&session.id).await.expect("touch"));
}

#[tokio::test]
async fn metadata_round_trips() {
    let store = open_in_memory().await.expect("open");
    let session = store
        .create_session(
            &identity(Some("/repo")),
            NewSessionMetadata {
                channel: Some("chat".into()),
                priority: Some("high".into()),
                tags: vec!["focus".into(), "build".into()],
            },
        )
        .await
        .expect("create");

    let got = store
        .get_session(&session.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.channel.as_deref(), Some("chat"));
    assert_eq!(got.priority.as_deref(), Some("high"));
    assert_eq!(got.tags, vec!["focus", "build"]);
    assert_eq!(got.title, "");
    assert!(got.ended_at_unix_ms.is_none());
}

#[tokio::test]
async fn most_recent_session_wins_when_multiple_active() {
    // Older active sessions can hang around if a previous run forgot to end
    // them; discovery must still pick the newest one.
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let _older = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("older");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newer = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("newer");
    let found = store
        .find_active_session(&id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id, newer.id);
}
