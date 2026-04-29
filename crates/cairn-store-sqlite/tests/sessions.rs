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
use cairn_store_sqlite::{NewSessionMetadata, ResolveOutcome, open, open_in_memory};

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
async fn migration_dedupes_preexisting_active_null_project_duplicates() {
    // Migration 0012 created the unique index that treats NULL as distinct,
    // so a vault that hit the original §8.1 race could carry multiple
    // active rows for the same (user, agent, project_root=NULL). Migration
    // 0013's stricter index would otherwise abort migration on those
    // vaults. The dedup step must keep the most recent row and end the
    // others. Simulate this by running migrations 1..=12, seeding the
    // duplicate state, then running migration 13 and asserting open
    // succeeds with one active row.
    use rusqlite_migration::{M, Migrations};

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    // Stage 1: open the DB at migration 12 (no unique index over NULL).
    {
        let mut conn = rusqlite::Connection::open(&db_path).expect("conn");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             PRAGMA foreign_keys=ON; \
             PRAGMA busy_timeout=5000;",
        )
        .expect("pragmas");
        let migrations = Migrations::new(vec![
            M::up(include_str!("../src/migrations/sql/0001_records.sql")),
            M::up(include_str!("../src/migrations/sql/0002_wal.sql")),
            M::up(include_str!("../src/migrations/sql/0003_replay.sql")),
            M::up(include_str!("../src/migrations/sql/0004_locks.sql")),
            M::up(include_str!("../src/migrations/sql/0005_consent.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0006_drift_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0007_tombstone_reason.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0008_record_extensions.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0011_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0012_sessions_unique_active.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 12");

        // Seed two active rows for the same vault-only identity at the
        // same NULL project_root — legal at this schema, illegal under 13.
        for (sid, last) in [("S_OLD", 100i64), ("S_NEW", 200i64)] {
            conn.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, 'usr:alice', 'agt:cli:x:y:v1', NULL, '', ?2, ?2, NULL)",
                rusqlite::params![sid, last],
            )
            .expect("insert");
        }
    }

    // Stage 2: open via the production helper, which runs migration 13
    // (and any future ones). The dedup must succeed.
    let store = open(&db_path).await.expect("open after dedup");

    // The newer row (S_NEW, last_activity=200) wins; the older (S_OLD,
    // last_activity=100) is ended.
    let id = SessionIdentity::new(
        Identity::parse("usr:alice").expect("user"),
        Identity::parse("agt:cli:x:y:v1").expect("agent"),
        None,
    )
    .expect("identity");
    let found = store
        .find_active_session(&id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id.as_str(), "S_NEW");

    // Sanity: the older row is no longer touchable (ended_at set).
    let old_id = cairn_core::domain::session::SessionId::parse("S_OLD").expect("parse");
    assert!(!store.touch_session(&old_id).await.expect("touch"));
}

#[tokio::test]
async fn cross_connection_resolvers_converge_on_one_session() {
    // Use two independently-opened stores against the same on-disk DB to
    // exercise real cross-connection contention. BEGIN IMMEDIATE on one
    // connection while the other holds the write lock raises SQLITE_BUSY
    // before the in-tx body ever runs; the resolver must retry that
    // acquisition failure rather than surface it as a terminal Worker
    // error.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");

    // Open once first to apply migrations + pragmas.
    {
        let _seed = open(&db).await.expect("first open");
    }

    let store_a = std::sync::Arc::new(open(&db).await.expect("open a"));
    let store_b = std::sync::Arc::new(open(&db).await.expect("open b"));
    let id = identity(Some("/repo"));

    let mut handles = Vec::new();
    for i in 0..32 {
        let store = if i % 2 == 0 {
            std::sync::Arc::clone(&store_a)
        } else {
            std::sync::Arc::clone(&store_b)
        };
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            store
                .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
                .await
                .expect("resolve must converge through retries, not surface BUSY")
                .into_session()
                .id
        }));
    }

    let mut session_ids = std::collections::HashSet::new();
    for h in handles {
        session_ids.insert(h.await.expect("join"));
    }
    assert_eq!(
        session_ids.len(),
        1,
        "cross-connection resolvers must converge on one session id, got {session_ids:?}",
    );
}

#[tokio::test]
async fn end_after_reuse_select_does_not_return_dead_session() {
    // The dangerous interleaving: resolve_or_create reads an active row,
    // decides reuse (within window), then a concurrent end_session closes
    // it before the bump UPDATE lands. Without a CAS on the reuse update,
    // resolve_or_create would return a session id whose row is already
    // ended. With CAS, the UPDATE matches zero rows, the tx restarts, and
    // the next iteration sees ended_at IS NOT NULL → CreateNew.
    //
    // Race many resolves against many ends; assert no resolver returns an
    // already-ended id.
    let store = std::sync::Arc::new(open_in_memory().await.expect("open"));
    let id = identity(Some("/repo"));
    // Seed an active row the enders can target.
    let seed = store
        .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
        .await
        .expect("seed");

    let mut handles: Vec<tokio::task::JoinHandle<Option<cairn_core::domain::Session>>> = Vec::new();
    for _ in 0..16 {
        let store = std::sync::Arc::clone(&store);
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            Some(
                store
                    .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
                    .await
                    .expect("resolve")
                    .into_session(),
            )
        }));
    }
    let seed_id = seed.into_session().id;
    for _ in 0..16 {
        let store = std::sync::Arc::clone(&store);
        let seed_id = seed_id.clone();
        handles.push(tokio::spawn(async move {
            let _ = store.end_session(&seed_id).await;
            None
        }));
    }

    // Collect resolver-returned sessions; the ender tasks return None.
    let mut resolver_sessions = Vec::new();
    for h in handles {
        if let Some(s) = h.await.expect("join") {
            resolver_sessions.push(s);
        }
    }
    // The atomic resolve tx selects under `ended_at IS NULL` and only
    // bumps under that same predicate (CAS). The Session returned by
    // resolve therefore reflects the in-tx snapshot — its
    // `ended_at_unix_ms` must be None even if a concurrent end_session
    // closed the row immediately after our tx committed.
    for session in resolver_sessions {
        assert!(
            session.ended_at_unix_ms.is_none(),
            "resolver returned a session with ended_at set: {session:?}",
        );
    }
}

#[tokio::test]
async fn touch_after_stale_select_keeps_session_alive_under_race() {
    // The dangerous interleaving is: resolve_or_create snapshots a stale
    // last_activity_at, then a concurrent caller touch_session()s the same
    // row, then resolve_or_create reaches the close UPDATE. Without a
    // compare-and-swap on last_activity_at, resolve_or_create would end a
    // freshly-active session and mint a replacement. With the CAS, the
    // close UPDATE matches zero rows, the tx restarts, and the next SELECT
    // sees the bumped activity → reuse.
    //
    // We can't deterministically schedule that interleaving from outside,
    // but we can race many touchers against many resolvers and assert the
    // invariant: at most one active session per identity, at any time.
    let store = std::sync::Arc::new(open_in_memory().await.expect("open"));
    let id = identity(Some("/repo"));
    let seed = store
        .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
        .await
        .expect("seed");
    let seed_id = seed.session().id.clone();

    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = std::sync::Arc::clone(&store);
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            store
                .resolve_or_create_session(&id, 86_400, NewSessionMetadata::default())
                .await
                .expect("resolve")
                .into_session()
                .id
        }));
    }
    for _ in 0..16 {
        let store = std::sync::Arc::clone(&store);
        let seed_id = seed_id.clone();
        handles.push(tokio::spawn(async move {
            // Touch is best-effort — return the seed id either way.
            let _ = store.touch_session(&seed_id).await;
            seed_id
        }));
    }

    let mut session_ids = std::collections::HashSet::new();
    for h in handles {
        session_ids.insert(h.await.expect("join"));
    }
    assert_eq!(
        session_ids.len(),
        1,
        "live touches must not allow resolve to fork a new session: {session_ids:?}",
    );
}

#[tokio::test]
async fn empty_project_root_is_rejected_at_db_layer() {
    // Migration 0013 installs BEFORE INSERT/UPDATE triggers that reject
    // project_root = '' so an empty string can never re-introduce the
    // NULL-vs-'' fragmentation that the coalesce-index closes. Direct
    // construction goes through SessionIdentity::new (which already rejects
    // empty), so this test reaches behind the API by upserting via the
    // sync test helper to confirm the DB-level guard fires.
    use cairn_store_sqlite::open_in_memory_sync;
    let conn = open_in_memory_sync().expect("open sync");
    let res = conn.execute(
        "INSERT INTO sessions (session_id, user_id, agent_id, project_root, title, \
                              created_at, last_activity_at, ended_at) \
         VALUES ('S1', 'usr:alice', 'agt:foo:bar:baz:v1', '', '', 0, 0, NULL)",
        [],
    );
    let err = res.expect_err("empty project_root must be rejected by trigger");
    let msg = format!("{err}");
    assert!(
        msg.contains("project_root"),
        "expected project_root guard error, got {msg}",
    );
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

#[tokio::test]
async fn explicit_session_resolution_rejects_foreign_identity() {
    // Alice creates a session under /repo. Bob (different usr:) hands over
    // alice's session id — perhaps copied from the env, leaked through a
    // hostile harness, or just a dangling CAIRN_SESSION_ID. The store
    // must refuse to operate on alice's row under bob's identity.
    let store = open_in_memory().await.expect("open");

    let alice = SessionIdentity::new(
        Identity::parse("usr:alice").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("alice identity");
    let bob = SessionIdentity::new(
        Identity::parse("usr:bob").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("bob identity");

    let session = store
        .create_session(&alice, NewSessionMetadata::default())
        .await
        .expect("create");

    // Alice resolving her own id succeeds and bumps activity.
    let resolved = store
        .resolve_explicit_session(&session.id, &alice)
        .await
        .expect("alice ok");
    assert!(resolved.is_some());

    // Bob using alice's id is rejected as identity mismatch — not a
    // missing-row, not an internal error.
    let err = store
        .resolve_explicit_session(&session.id, &bob)
        .await
        .expect_err("bob's call must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("identity mismatch"),
        "expected SessionIdentityMismatch, got {msg}",
    );
}

#[tokio::test]
async fn explicit_session_resolution_returns_none_for_ended_or_missing() {
    let store = open_in_memory().await.expect("open");

    let alice = SessionIdentity::new(
        Identity::parse("usr:alice").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("identity");

    // Missing id → Ok(None), not an error. Lets the dispatcher fall through
    // to auto-discover.
    let unknown = cairn_core::domain::session::SessionId::parse("01HXMISSING0000000000000001")
        .expect("parse");
    let got = store
        .resolve_explicit_session(&unknown, &alice)
        .await
        .expect("ok");
    assert!(got.is_none());

    // Ended session also returns None — operating on a corpse would let
    // the caller resurrect a closed session via touch.
    let session = store
        .create_session(&alice, NewSessionMetadata::default())
        .await
        .expect("create");
    assert!(store.end_session(&session.id).await.expect("end"));
    let got = store
        .resolve_explicit_session(&session.id, &alice)
        .await
        .expect("ok");
    assert!(got.is_none(), "ended session must surface as None");
}
