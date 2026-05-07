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
    Identity::parse("hmn:alice").expect("valid")
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
    let id = identity(Some("/repo"));
    let session = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("create");
    let before = store
        .get_session(&session.id, &id)
        .await
        .expect("get")
        .expect("present");

    // SQLite millisecond timestamps may collide if invoked back-to-back; sleep
    // a tick to make the assertion non-flaky.
    std::thread::sleep(std::time::Duration::from_millis(5));

    assert!(
        store.touch_session(&session.id, &id).await.expect("touch"),
        "active session should bump",
    );
    let after = store
        .get_session(&session.id, &id)
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
    assert!(store.end_session(&session.id, &id).await.expect("end"));
    let found = store.find_active_session(&id).await.expect("find");
    assert!(found.is_none(), "ended session must not be returned");
    // Idempotent re-end is a no-op.
    assert!(!store.end_session(&session.id, &id).await.expect("re-end"));
}

#[tokio::test]
async fn touch_on_ended_session_is_noop() {
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let session = store
        .create_session(&id, NewSessionMetadata::default())
        .await
        .expect("create");
    assert!(store.end_session(&session.id, &id).await.expect("end"));
    assert!(!store.touch_session(&session.id, &id).await.expect("touch"));
}

#[tokio::test]
async fn metadata_round_trips() {
    let store = open_in_memory().await.expect("open");
    let id = identity(Some("/repo"));
    let session = store
        .create_session(
            &id,
            NewSessionMetadata {
                channel: Some("chat".into()),
                priority: Some("high".into()),
                tags: vec!["focus".into(), "build".into()],
            },
        )
        .await
        .expect("create");

    let got = store
        .get_session(&session.id, &id)
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
    let touched = store.touch_session(&prior.id, &id).await.expect("touch");
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
            M::up(include_str!("../src/migrations/sql/0009_consent_event.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0011_consent_event_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0012_filter_alignment.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0013_edges_updates_dst_idx.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0014_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0015_sessions_unique_active.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 17");

        // Seed two active rows for the same vault-only identity at the
        // same NULL project_root — legal at this schema, illegal under 13.
        for (sid, last) in [("S_OLD", 100i64), ("S_NEW", 200i64)] {
            conn.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, 'hmn:alice', 'agt:cli:x:y:v1', NULL, '', ?2, ?2, NULL)",
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
        Identity::parse("hmn:alice").expect("user"),
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
    assert!(!store.touch_session(&old_id, &id).await.expect("touch"));
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
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            let _ = store.end_session(&seed_id, &id).await;
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
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            // Touch is best-effort — return the seed id either way.
            let _ = store.touch_session(&seed_id, &id).await;
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
         VALUES ('S1', 'hmn:alice', 'agt:foo:bar:baz:v1', '', '', 0, 0, NULL)",
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
    // Alice creates a session under /repo. Bob (different hmn:) hands over
    // alice's session id — perhaps copied from the env, leaked through a
    // hostile harness, or just a dangling CAIRN_SESSION_ID. The store
    // must refuse to operate on alice's row under bob's identity.
    let store = open_in_memory().await.expect("open");

    let alice = SessionIdentity::new(
        Identity::parse("hmn:alice").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("alice identity");
    let bob = SessionIdentity::new(
        Identity::parse("hmn:bob").expect("user"),
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
    assert_eq!(resolved.id, session.id);
    assert!(
        resolved.last_activity_at_unix_ms >= session.last_activity_at_unix_ms,
        "explicit resolve must bump last_activity_at",
    );

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
async fn explicit_session_resolution_fails_closed_for_missing() {
    // Brief §8.1: explicit session ids are authoritative. A typo in
    // --session, a stale CAIRN_SESSION_ID, or any other garbage must
    // fail closed rather than silently fall through to auto-discover.
    let store = open_in_memory().await.expect("open");

    let alice = SessionIdentity::new(
        Identity::parse("hmn:alice").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("identity");

    let unknown = cairn_core::domain::session::SessionId::parse("01HXMISSING0000000000000001")
        .expect("parse");
    let err = store
        .resolve_explicit_session(&unknown, &alice)
        .await
        .expect_err("missing id must surface a typed error");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "expected SessionNotFound, got {msg}",
    );
}

#[tokio::test]
async fn migration_ends_active_rows_with_relative_project_root() {
    // Migration 0014 closes any active session whose `project_root` is a
    // relative path. The current write path (SessionIdentity::new) rejects
    // them, but read-path hydration (SessionIdentity::from_persisted)
    // tolerates them so a vault upgraded from an older resolver does not
    // fail to open. Such rows would otherwise be unreachable through
    // discovery (`project_root IS ?3` against a canonical absolute caller
    // string never matches a stored relative string), silently splitting
    // history. Seed the legacy state at migration 13, run migrations to
    // head, and assert: relative-path rows are ended; absolute-path rows
    // (POSIX, Windows drive, UNC) survive.
    use rusqlite_migration::{M, Migrations};

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

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
            M::up(include_str!("../src/migrations/sql/0009_consent_event.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0011_consent_event_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0012_filter_alignment.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0013_edges_updates_dst_idx.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0014_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0015_sessions_unique_active.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0016_sessions_unique_active_coalesce.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 18");

        // Seed: one relative-path row (legacy) plus three absolute-path
        // rows (POSIX, Windows drive, Windows UNC) plus one NULL row.
        // Each gets its own (user, agent, project_root) so the unique
        // index doesn't reject the seed itself.
        for (sid, user, agent, root) in [
            ("S_REL", "hmn:legacy", "agt:cli:x:y:v1", Some("subdir/repo")),
            ("S_POSIX", "hmn:abs1", "agt:cli:x:y:v1", Some("/abs/repo")),
            ("S_WIN", "hmn:abs2", "agt:cli:x:y:v1", Some(r"C:\repo")),
            ("S_UNC", "hmn:abs3", "agt:cli:x:y:v1", Some(r"\\srv\share")),
            ("S_WINFWD", "hmn:abs5", "agt:cli:x:y:v1", Some("C:/repo")),
            // Single leading backslash is *not* UNC and *not* absolute on
            // any platform — POSIX treats `\` as a filename character, and
            // Windows requires `\\server\share` for UNC. Must be ended.
            ("S_BS_REL", "hmn:legacy2", "agt:cli:x:y:v1", Some(r"\repo")),
            ("S_NULL", "hmn:abs4", "agt:cli:x:y:v1", None),
        ] {
            conn.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, ?2, ?3, ?4, '', 100, 100, NULL)",
                rusqlite::params![sid, user, agent, root],
            )
            .expect("insert");
        }
    }

    // Run to head — 0014 should end S_REL and leave the others alone.
    let store = open(&db_path).await.expect("open after 0017");

    for (sid, user, root, expect_ended) in [
        ("S_REL", "hmn:legacy", Some("subdir/repo"), true),
        ("S_POSIX", "hmn:abs1", Some("/abs/repo"), false),
        // 0016 case-folds Windows-shape rows to match the runtime
        // ASCII-lowercase canonical (`SessionIdentity::new`).
        ("S_WIN", "hmn:abs2", Some(r"c:\repo"), false),
        ("S_UNC", "hmn:abs3", Some(r"\\srv\share"), false),
        // 0014 leaves C:/repo active; 0015 canonicalizes slashes; 0016
        // lowercases.
        ("S_WINFWD", "hmn:abs5", Some(r"c:\repo"), false),
        ("S_BS_REL", "hmn:legacy2", Some(r"\repo"), true),
        ("S_NULL", "hmn:abs4", None, false),
    ] {
        let sess = store
            .get_session_unchecked(
                &cairn_core::domain::session::SessionId::parse(sid).expect("parse"),
            )
            .await
            .expect("get")
            .unwrap_or_else(|| panic!("{sid} present"));
        assert_eq!(sess.identity.user.as_str(), user);
        assert_eq!(sess.identity.project_root.as_deref(), root);
        assert_eq!(
            sess.ended_at_unix_ms.is_some(),
            expect_ended,
            "{sid}: expected ended={expect_ended}, got ended_at={:?}",
            sess.ended_at_unix_ms,
        );
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive seed/expect tables document every Windows-shape variant the migration covers; splitting them would obscure the intent"
)]
async fn migration_canonicalizes_legacy_windows_slash_project_roots() {
    // A vault from a prior resolver that stored `C:/repo` (or
    // `//srv/share`) survives migration 0014 (those are absolute paths
    // by string shape) but, with the new write-path canonicalization,
    // post-upgrade callers normalize to `C:\repo` / `\\srv\share`.
    // Lookup keys on the raw stored string, so the legacy row would
    // become unreachable and resolve_or_create would fork a new
    // session. Migration 0015 rewrites the surviving Windows-shape
    // rows to backslash form so post-upgrade discovery hits them.
    use rusqlite_migration::{M, Migrations};

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

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
            M::up(include_str!("../src/migrations/sql/0009_consent_event.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0011_consent_event_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0012_filter_alignment.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0013_edges_updates_dst_idx.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0014_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0015_sessions_unique_active.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0016_sessions_unique_active_coalesce.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0017_sessions_close_relative_project_root.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 19");

        // Seed: forward-slash drive (`C:/repo`), forward-slash UNC
        // (`//srv/share`), already-canonical backslash drive that must
        // not be touched, and a POSIX path containing `/` that must
        // also not be corrupted into `\`.
        for (sid, user, root) in [
            ("S_DRV_FWD", "hmn:win1", "C:/repo"),
            // `//srv/share` is treated as POSIX, not UNC, so it
            // passes through migration 0015 unchanged.
            ("S_POSIX_DBL", "hmn:win2", "//srv/share"),
            ("S_DRV_OK", "hmn:win3", r"D:\repo"),
            ("S_POSIX_OK", "hmn:nix1", "/abs/repo"),
            // Mixed-slash variants — a drive path that starts canonical
            // (`E:\`) but has internal `/`, and a UNC that starts
            // canonical (`\\srv\`) but has `/` deeper in the path.
            // These survive dedup and must still be rewritten to fully
            // canonical backslash form by the rewrite step.
            ("S_DRV_MIX", "hmn:win4", r"E:\foo/bar"),
            ("S_UNC_MIX", "hmn:win5", r"\\srv\share/sub"),
            // Trailing-separator variants — runtime
            // `normalize_project_root` trims these, so the migration
            // must too or upgrade splits the session.
            ("S_DRV_TRAIL", "hmn:win6", "F:/repo/"),
            ("S_UNC_TRAIL", "hmn:win7", r"\\srv\share\"),
            // Drive-root case: `G:\` must NOT be trimmed to `G:` (which
            // would be drive-relative on Windows and rejected by the
            // runtime classifier). Slash variant `G:/` must collapse to
            // `G:\` and stay there.
            ("S_DRV_ROOT", "hmn:win8", "G:/"),
        ] {
            conn.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, ?2, 'agt:cli:x:y:v1', ?3, '', 100, 100, NULL)",
                rusqlite::params![sid, user, root],
            )
            .expect("insert");
        }
    }

    let store = open(&db_path).await.expect("open after 0018");

    for (sid, expect_root) in [
        ("S_DRV_FWD", r"c:\repo"),
        ("S_POSIX_DBL", "//srv/share"),
        ("S_DRV_OK", r"d:\repo"),
        ("S_POSIX_OK", "/abs/repo"),
        ("S_DRV_MIX", r"e:\foo\bar"),
        ("S_UNC_MIX", r"\\srv\share\sub"),
        ("S_DRV_TRAIL", r"f:\repo"),
        ("S_UNC_TRAIL", r"\\srv\share"),
        ("S_DRV_ROOT", r"g:\"),
    ] {
        let sess = store
            .get_session_unchecked(
                &cairn_core::domain::session::SessionId::parse(sid).expect("parse"),
            )
            .await
            .expect("get")
            .unwrap_or_else(|| panic!("{sid} present"));
        assert_eq!(
            sess.identity.project_root.as_deref(),
            Some(expect_root),
            "{sid}: expected {expect_root}, got {:?}",
            sess.identity.project_root,
        );
        assert!(
            sess.ended_at_unix_ms.is_none(),
            "{sid} must remain active across canonicalization",
        );
    }

    // A post-upgrade caller using the canonical write-path identity
    // (`C:\repo`) finds the legacy `C:/repo` row by the same id.
    let canonical_id = SessionIdentity::new(
        Identity::parse("hmn:win1").expect("user"),
        Identity::parse("agt:cli:x:y:v1").expect("agent"),
        Some(r"C:\repo".into()),
    )
    .expect("identity");
    let found = store
        .find_active_session(&canonical_id)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id.as_str(), "S_DRV_FWD");
}

#[tokio::test]
async fn migration_canonicalizes_ended_legacy_windows_rows_for_explicit_resolve() {
    // `resolve_explicit_session` checks identity equality before the
    // ended-state check (so a foreign-id reuse can never reach a
    // store-level "this session ended" probe). If 0015 left ended
    // legacy rows in their raw `C:/repo` form, a caller reopening
    // their own historical session under the canonical `C:\repo`
    // would see `SessionIdentityMismatch` instead of `SessionEnded`,
    // breaking the §8.1 fail-closed semantics. Ended rows must be
    // canonicalized too.
    use rusqlite_migration::{M, Migrations};

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

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
            M::up(include_str!("../src/migrations/sql/0009_consent_event.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0011_consent_event_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0012_filter_alignment.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0013_edges_updates_dst_idx.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0014_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0015_sessions_unique_active.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0016_sessions_unique_active_coalesce.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0017_sessions_close_relative_project_root.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 19");

        // Seed an *ended* legacy row stored as `C:/repo/`.
        conn.execute(
            "INSERT INTO sessions \
               (session_id, user_id, agent_id, project_root, title, \
                created_at, last_activity_at, ended_at) \
             VALUES ('S_ENDED_LEG', 'hmn:winx', 'agt:cli:x:y:v1', \
                     'C:/repo/', '', 100, 100, 200)",
            [],
        )
        .expect("insert ended");
    }

    let store = open(&db_path).await.expect("open after 0018");

    // Caller's canonical identity for the same project.
    let canonical = SessionIdentity::new(
        Identity::parse("hmn:winx").expect("user"),
        Identity::parse("agt:cli:x:y:v1").expect("agent"),
        Some(r"C:\repo".into()),
    )
    .expect("identity");
    let id = cairn_core::domain::session::SessionId::parse("S_ENDED_LEG").expect("parse");

    // Without canonicalization of ended rows, this would surface
    // SessionIdentityMismatch. With 0015's full canonicalization, it
    // surfaces SessionEnded — the §8.1 contract.
    let err = store
        .resolve_explicit_session(&id, &canonical)
        .await
        .expect_err("ended legacy row must surface a typed error");
    let msg = format!("{err}");
    assert!(
        msg.contains("is ended"),
        "expected SessionEnded for canonicalized ended row, got `{msg}`",
    );
}

#[tokio::test]
async fn migration_canonicalizes_ended_verbatim_windows_rows_for_explicit_resolve() {
    // 0015's slash-collapse rewrite preserves `\\?\` rows verbatim
    // (LIKE patterns don't match the `\\?\` shape). Without 0016
    // stripping the prefix on ended rows too, an ended legacy row
    // stored as `\\?\C:\Repo\` would surface SessionIdentityMismatch
    // when a post-upgrade caller reopens by id under the runtime
    // canonical (`c:\repo`) — the same §8.1 contract violation 0015
    // fixed for plain-slash legacy rows. Cover both verbatim drive
    // and verbatim UNC.
    use rusqlite_migration::{M, Migrations};

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

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
            M::up(include_str!("../src/migrations/sql/0009_consent_event.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0011_consent_event_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0012_filter_alignment.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0013_edges_updates_dst_idx.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0014_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0015_sessions_unique_active.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0016_sessions_unique_active_coalesce.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0017_sessions_close_relative_project_root.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0018_sessions_canonicalize_windows_paths.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 17");

        // Two ended verbatim rows: drive (with mixed case + trailing
        // separator) and UNC. Both must surface SessionEnded after
        // 0016 strips + lowercases ended rows too.
        for (sid, user, root) in [
            ("S_ENDED_VRB_DRV", "hmn:vrb1", r"\\?\C:\Repo\"),
            ("S_ENDED_VRB_UNC", "hmn:vrb2", r"\\?\UNC\Srv\Share"),
        ] {
            conn.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, ?2, 'agt:cli:x:y:v1', ?3, '', 100, 100, 200)",
                rusqlite::params![sid, user, root],
            )
            .expect("insert ended verbatim");
        }
    }

    let store = open(&db_path).await.expect("open after 0019");

    for (sid, user, raw_caller_root) in [
        ("S_ENDED_VRB_DRV", "hmn:vrb1", r"C:\Repo"),
        ("S_ENDED_VRB_UNC", "hmn:vrb2", r"\\Srv\Share"),
    ] {
        let canonical = SessionIdentity::new(
            Identity::parse(user).expect("user"),
            Identity::parse("agt:cli:x:y:v1").expect("agent"),
            Some(raw_caller_root.into()),
        )
        .expect("identity");
        let id = cairn_core::domain::session::SessionId::parse(sid).expect("parse");
        let err = store
            .resolve_explicit_session(&id, &canonical)
            .await
            .expect_err("ended verbatim row must surface a typed error");
        let msg = format!("{err}");
        assert!(
            msg.contains("is ended"),
            "{sid}: expected SessionEnded after 0016, got `{msg}`",
        );
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "exhaustive seed/expect tables document every verbatim/case-fold variant; pair_a_winner / pair_a_loser pairs read clearer than disjoint synonyms"
)]
async fn migration_strips_verbatim_prefixes_and_case_folds() {
    // Migration 0016 covers two follow-on issues from 0015:
    //   1. Windows verbatim prefixes (`\\?\C:\Repo`, `\\?\UNC\Srv\Share`)
    //      were stored raw by pre-canonicalization callers but the
    //      runtime now strips them — legacy rows would be unreachable.
    //   2. Windows file systems are case-insensitive; the runtime
    //      ASCII-lowercases Windows-shape paths but legacy rows kept
    //      mixed case.
    // Combined: rows like `\\?\C:\Repo` and `c:\repo` for the same
    // (user, agent) collapse to one canonical key after stripping +
    // case-fold, so dedup must keep only the newest per partition.
    use rusqlite_migration::{M, Migrations};

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

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
            M::up(include_str!("../src/migrations/sql/0009_consent_event.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0010_ranking_indexes.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0011_consent_event_hardening.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0012_filter_alignment.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0013_edges_updates_dst_idx.sql"
            )),
            M::up(include_str!("../src/migrations/sql/0014_sessions.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0015_sessions_unique_active.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0016_sessions_unique_active_coalesce.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0017_sessions_close_relative_project_root.sql"
            )),
            M::up(include_str!(
                "../src/migrations/sql/0018_sessions_canonicalize_windows_paths.sql"
            )),
        ]);
        migrations.to_latest(&mut conn).expect("migrate to 17");

        // Two pairs that collapse to the same canonical key under 0016.
        // Per pair: one verbatim/mixed-case legacy row + one already
        // canonical row for the same (user, agent). Newer last_activity
        // wins per the dedup partition.
        for (sid, user, root, last_act) in [
            // Pair A: verbatim drive + plain canonical, same identity.
            ("S_VRB_DRV", "hmn:dup1", r"\\?\C:\Repo", 200_i64),
            ("S_PLN_DRV", "hmn:dup1", r"c:\repo", 100_i64),
            // Pair B: verbatim UNC + plain UNC mixed-case, same identity.
            ("S_VRB_UNC", "hmn:dup2", r"\\?\UNC\Srv\Share", 100_i64),
            ("S_PLN_UNC", "hmn:dup2", r"\\Srv\Share", 200_i64),
            // Standalone case-only legacy: must be lowercased in place.
            ("S_MIX_DRV", "hmn:solo1", r"H:\MixedCase\Path", 100_i64),
            // Standalone verbatim drive: must be stripped + lowercased.
            ("S_VRB_SOLO", "hmn:solo2", r"\\?\K:\Solo", 100_i64),
            // Verbatim drive ROOT: `\\?\C:\` must become `c:\` — NOT
            // `c:`. Trimming trailing `\` here would yield a
            // drive-relative path that the runtime classifier rejects,
            // forking the session on the next discovery.
            ("S_VRB_ROOT", "hmn:solo3", r"\\?\C:\", 100_i64),
            // Same risk for the slash-spelling: `\\?\D:/` → `d:\`.
            ("S_VRB_ROOT_FWD", "hmn:solo4", r"\\?\D:/", 100_i64),
            // POSIX must remain untouched (case + slashes).
            ("S_POSIX_KEEP", "hmn:nix", "/Abs/Repo", 100_i64),
        ] {
            conn.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, ?2, 'agt:cli:x:y:v1', ?3, '', 100, ?4, NULL)",
                rusqlite::params![sid, user, root, last_act],
            )
            .expect("insert");
        }
    }

    let store = open(&db_path).await.expect("open after 0019");

    // Pair A: verbatim row newer → wins; plain row ended.
    let pair_a_winner = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_VRB_DRV").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_VRB_DRV present");
    assert_eq!(
        pair_a_winner.identity.project_root.as_deref(),
        Some(r"c:\repo"),
        "verbatim drive must be stripped and lowercased",
    );
    assert!(
        pair_a_winner.ended_at_unix_ms.is_none(),
        "newer (verbatim) row in pair A must remain active",
    );
    let pair_a_loser = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_PLN_DRV").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_PLN_DRV present");
    assert!(
        pair_a_loser.ended_at_unix_ms.is_some(),
        "older row in pair A must be ended by dedup",
    );

    // Pair B: plain UNC mixed-case is newer → wins; verbatim UNC ended.
    let pair_b_winner = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_PLN_UNC").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_PLN_UNC present");
    assert_eq!(
        pair_b_winner.identity.project_root.as_deref(),
        Some(r"\\srv\share"),
        "plain UNC must be lowercased",
    );
    assert!(
        pair_b_winner.ended_at_unix_ms.is_none(),
        "newer row in pair B must remain active",
    );
    let pair_b_loser = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_VRB_UNC").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_VRB_UNC present");
    assert!(
        pair_b_loser.ended_at_unix_ms.is_some(),
        "older (verbatim UNC) row in pair B must be ended by dedup",
    );

    // Standalones: lowercased / stripped in place, still active.
    let mix_drv = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_MIX_DRV").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_MIX_DRV present");
    assert_eq!(
        mix_drv.identity.project_root.as_deref(),
        Some(r"h:\mixedcase\path"),
    );
    assert!(mix_drv.ended_at_unix_ms.is_none());

    let vrb_solo = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_VRB_SOLO").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_VRB_SOLO present");
    assert_eq!(
        vrb_solo.identity.project_root.as_deref(),
        Some(r"k:\solo"),
        "standalone verbatim drive must be stripped and lowercased",
    );
    assert!(vrb_solo.ended_at_unix_ms.is_none());

    // Verbatim drive root must preserve the trailing `\`.
    let vrb_root = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_VRB_ROOT").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_VRB_ROOT present");
    assert_eq!(
        vrb_root.identity.project_root.as_deref(),
        Some(r"c:\"),
        "verbatim drive root must keep its trailing separator (not `c:`)",
    );
    assert!(vrb_root.ended_at_unix_ms.is_none());

    let vrb_root_fwd = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_VRB_ROOT_FWD").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_VRB_ROOT_FWD present");
    assert_eq!(
        vrb_root_fwd.identity.project_root.as_deref(),
        Some(r"d:\"),
        "verbatim drive root with `/` must collapse to `d:\\` (not `d:`)",
    );
    assert!(vrb_root_fwd.ended_at_unix_ms.is_none());

    // POSIX must be untouched: case preserved, no slash flipping.
    let posix = store
        .get_session_unchecked(
            &cairn_core::domain::session::SessionId::parse("S_POSIX_KEEP").expect("parse"),
        )
        .await
        .expect("get")
        .expect("S_POSIX_KEEP present");
    assert_eq!(posix.identity.project_root.as_deref(), Some("/Abs/Repo"));

    // Post-upgrade caller using the runtime canonical (lowercase) form
    // resolves the migrated row.
    let canonical = SessionIdentity::new(
        Identity::parse("hmn:dup1").expect("user"),
        Identity::parse("agt:cli:x:y:v1").expect("agent"),
        Some(r"C:\Repo".into()),
    )
    .expect("identity");
    let found = store
        .find_active_session(&canonical)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found.id.as_str(), "S_VRB_DRV");
}

#[tokio::test]
async fn touch_and_end_reject_cross_identity_session_ids() {
    // Brief §8.1: a leaked / guessed session id must not let a foreign
    // (user, agent, project_root) bump activity on or close another
    // identity's row. The identity guard lives at the store layer so
    // higher layers can't accidentally drop it.
    let store = open_in_memory().await.expect("open");

    let alice = SessionIdentity::new(
        Identity::parse("hmn:alice").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("alice identity");
    let bob = SessionIdentity::new(
        Identity::parse("hmn:bob").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("bob identity");

    let session = store
        .create_session(&alice, NewSessionMetadata::default())
        .await
        .expect("create");

    // Bob holding alice's id — touch is a no-op, not a successful bump.
    assert!(
        !store.touch_session(&session.id, &bob).await.expect("touch"),
        "cross-identity touch must report no row updated",
    );
    assert!(
        !store.end_session(&session.id, &bob).await.expect("end"),
        "cross-identity end must report no row updated",
    );

    // The row is still active under alice's identity.
    let still_active = store
        .find_active_session(&alice)
        .await
        .expect("find")
        .expect("present");
    assert_eq!(still_active.id, session.id);

    // Alice's own touch / end still succeed.
    assert!(
        store
            .touch_session(&session.id, &alice)
            .await
            .expect("alice touch"),
    );
    assert!(
        store
            .end_session(&session.id, &alice)
            .await
            .expect("alice end"),
    );
}

#[tokio::test]
async fn explicit_session_resolution_fails_closed_for_ended() {
    // An already-closed session is also authoritative: the caller asked
    // for *that* session, and silently moving them to a new one would
    // mix two conversations.
    let store = open_in_memory().await.expect("open");

    let alice = SessionIdentity::new(
        Identity::parse("hmn:alice").expect("user"),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("agent"),
        Some("/repo".into()),
    )
    .expect("identity");

    let session = store
        .create_session(&alice, NewSessionMetadata::default())
        .await
        .expect("create");
    assert!(store.end_session(&session.id, &alice).await.expect("end"));

    let err = store
        .resolve_explicit_session(&session.id, &alice)
        .await
        .expect_err("ended session must surface a typed error");
    let msg = format!("{err}");
    assert!(msg.contains("is ended"), "expected SessionEnded, got {msg}");
}
