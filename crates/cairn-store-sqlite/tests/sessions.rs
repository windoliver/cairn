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
use cairn_store_sqlite::{NewSessionMetadata, ResolveOutcome, open_in_memory};

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
async fn second_create_session_for_same_identity_violates_unique_index() {
    // The partial unique index `sessions_one_active_per_identity_idx`
    // enforces the §8.1 invariant that a single (user, agent, project_root)
    // resolves to one active session. Direct create after end is fine;
    // direct create over an active row is rejected. Callers should use
    // resolve_or_create_session to get the atomic resolve-or-create path.
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let _first = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("first");
    let err = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect_err("second create must hit the unique index");
    // Walk the error chain to find the underlying SQLite constraint error;
    // top-level Display is the wrapper variant.
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("UNIQUE") || dbg.contains("constraint"),
        "expected unique-constraint violation in error chain, got {dbg}",
    );
}

#[tokio::test]
async fn resolve_or_create_returns_created_for_first_call() {
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let outcome = store
        .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
        .await
        .expect("resolve");
    assert!(matches!(outcome, ResolveOutcome::Created(_)));
}

#[tokio::test]
async fn resolve_or_create_reuses_within_window() {
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let first = store
        .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
        .await
        .expect("first");
    let second = store
        .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
        .await
        .expect("second");
    assert!(matches!(second, ResolveOutcome::Reused(_)));
    assert_eq!(first.session().id, second.session().id);
}

#[tokio::test]
async fn resolve_or_create_closes_stale_row_before_creating_new() {
    // With idle_window_secs = 0 and elapsed time > 1 s, the prior row is
    // strictly past the window: resolve must end it and mint a new one. The
    // returned session must be different from the prior, and the prior id
    // must reject touch_session afterwards (cannot be revived).
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let prior = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("prior");

    // idle_secs is computed in whole seconds; sleep just over 1 s so the
    // floor-divided idle_secs strictly exceeds idle_window_secs = 0.
    std::thread::sleep(std::time::Duration::from_millis(1_100));

    let outcome = store
        .resolve_or_create_session(&id, 0, NewSessionMetadata::default())
        .await
        .expect("resolve");
    let ResolveOutcome::Created(new) = outcome else {
        panic!("expected Created when prior was past window, got {outcome:?}");
    };
    assert_ne!(new.id, prior.id);

    // Touch on the now-ended prior must fail — the §8.1 expiry invariant.
    let touched = store.touch_session(&prior.id).await.expect("touch");
    assert!(!touched, "expired session must not be revivable via touch");

    // Discovery returns the new id, not the closed prior.
    let found = store
        .find_active_session(&id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id, new.id);
}

#[tokio::test]
async fn concurrent_resolve_or_create_with_null_project_yields_one_session() {
    // SQLite unique indexes treat NULL as distinct, which would let two
    // racing inserts both succeed for vault-only (project_root = NULL)
    // identities. Migration 0013 closes that hole by coercing NULL to ''
    // inside the unique index. Without that fix, this test fragments into
    // multiple sessions; with it, all racers converge on one id.
    let store = std::sync::Arc::new(open_in_memory().await.expect("open"));
    let id = identity(None);

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let store = std::sync::Arc::clone(&store);
            let id = id.clone();
            tokio::spawn(async move {
                store
                    .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
                    .await
                    .expect("resolve")
            })
        })
        .collect();

    let mut session_ids = std::collections::HashSet::new();
    for h in handles {
        let outcome = h.await.expect("join");
        session_ids.insert(outcome.into_session().id);
    }
    assert_eq!(
        session_ids.len(),
        1,
        "vault-only concurrent resolvers must converge on one session id, got {session_ids:?}",
    );
}

#[tokio::test]
async fn concurrent_resolve_or_create_yields_one_session() {
    // Race many resolve_or_create_session calls in parallel. The partial
    // unique index forces all but one INSERT to fail; the loser tx
    // rollbacks and retries, observing the winner. Net result: one session.
    let store = std::sync::Arc::new(open_in_memory().await.expect("open"));
    let id = identity(Some("/repo"));

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let store = std::sync::Arc::clone(&store);
            let id = id.clone();
            tokio::spawn(async move {
                store
                    .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
                    .await
                    .expect("resolve")
            })
        })
        .collect();

    let mut session_ids = std::collections::HashSet::new();
    for h in handles {
        let outcome = h.await.expect("join");
        session_ids.insert(outcome.into_session().id);
    }
    assert_eq!(
        session_ids.len(),
        1,
        "all concurrent resolvers must converge on one session id, got {session_ids:?}",
    );
}
