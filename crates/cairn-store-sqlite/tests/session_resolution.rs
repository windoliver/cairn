//! SQLite-backed session lifecycle tests.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::session::{
    DEFAULT_IDLE_WINDOW_MILLIS, ResolveSessionRequest, SessionContext, SessionError,
    SessionIdCandidates, SessionMetadata, SessionResolutionSource,
};
use cairn_store_sqlite::SqliteMemoryStore;

fn request(project_id: Option<&str>, now_unix_millis: i64) -> ResolveSessionRequest {
    ResolveSessionRequest {
        candidates: SessionIdCandidates::default(),
        context: SessionContext {
            user_id: "usr:tafeng".to_owned(),
            agent_id: "agt:codex:v1".to_owned(),
            vault_id: Some("vault-local".to_owned()),
            project_id: project_id.map(str::to_owned),
            cwd: project_id.map(|id| format!("/work/{id}")),
        },
        metadata: SessionMetadata {
            channel: Some("hook".to_owned()),
            priority: Some("normal".to_owned()),
            tags: vec!["cli".to_owned()],
        },
        idle_window_millis: DEFAULT_IDLE_WINDOW_MILLIS,
        now_unix_millis,
    }
}

#[test]
fn first_call_in_new_project_creates_session_with_metadata() {
    let store = SqliteMemoryStore::open_in_memory().expect("in-memory sqlite store");

    let resolved = store
        .resolve_session(&request(Some("project-a"), 1_000))
        .expect("first session should be created");

    assert!(resolved.created);
    assert_eq!(resolved.source, SessionResolutionSource::AutoCreate);
    assert!(!resolved.session_id.is_empty());
    assert_eq!(resolved.record.title, "");
    assert_eq!(resolved.record.project_id.as_deref(), Some("project-a"));
    assert_eq!(resolved.record.channel.as_deref(), Some("hook"));
    assert_eq!(resolved.record.tags, vec!["cli"]);
}

#[test]
fn repeated_call_in_same_context_reuses_active_session() {
    let store = SqliteMemoryStore::open_in_memory().expect("in-memory sqlite store");

    let first = store
        .resolve_session(&request(Some("project-a"), 1_000))
        .expect("first session");
    let second = store
        .resolve_session(&request(Some("project-a"), 2_000))
        .expect("second session");

    assert!(!second.created);
    assert_eq!(second.source, SessionResolutionSource::AutoDiscovery);
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.record.last_activity_at_unix_millis, 2_000);
}

#[test]
fn new_project_creates_distinct_active_session() {
    let store = SqliteMemoryStore::open_in_memory().expect("in-memory sqlite store");

    let first = store
        .resolve_session(&request(Some("project-a"), 1_000))
        .expect("project-a session");
    let second = store
        .resolve_session(&request(Some("project-b"), 2_000))
        .expect("project-b session");

    assert_ne!(second.session_id, first.session_id);
    assert_eq!(second.record.project_id.as_deref(), Some("project-b"));
}

#[test]
fn idle_window_rollover_ends_old_session_and_creates_new_one() {
    let store = SqliteMemoryStore::open_in_memory().expect("in-memory sqlite store");
    let first = store
        .resolve_session(&request(Some("project-a"), 1_000))
        .expect("first session");

    let second = store
        .resolve_session(&request(
            Some("project-a"),
            1_000 + DEFAULT_IDLE_WINDOW_MILLIS + 1,
        ))
        .expect("rollover session");

    assert!(second.created);
    assert_ne!(second.session_id, first.session_id);

    let old = store
        .session_by_id(&first.session_id)
        .expect("lookup should work")
        .expect("old session should remain for audit");
    assert_eq!(
        old.ended_at_unix_millis,
        Some(1_000 + DEFAULT_IDLE_WINDOW_MILLIS + 1)
    );
}

#[test]
fn explicit_session_id_overrides_auto_discovery_and_is_created_if_absent() {
    let store = SqliteMemoryStore::open_in_memory().expect("in-memory sqlite store");
    let _active = store
        .resolve_session(&request(Some("project-a"), 1_000))
        .expect("active auto session");

    let mut explicit = request(Some("project-a"), 2_000);
    explicit.candidates.explicit_arg = Some("session-explicit".to_owned());
    let resolved = store
        .resolve_session(&explicit)
        .expect("explicit session should be accepted");

    assert!(resolved.created);
    assert_eq!(resolved.session_id, "session-explicit");
    assert_eq!(resolved.source, SessionResolutionSource::ExplicitArg);
}

#[test]
fn missing_project_is_ambiguous_when_multiple_active_sessions_match_user_and_agent() {
    let store = SqliteMemoryStore::open_in_memory().expect("in-memory sqlite store");
    store
        .resolve_session(&request(Some("project-a"), 1_000))
        .expect("project-a session");
    store
        .resolve_session(&request(Some("project-b"), 2_000))
        .expect("project-b session");

    let err = store
        .resolve_session(&request(None, 3_000))
        .expect_err("multiple active project sessions require explicit selection");

    assert!(matches!(err, SessionError::AmbiguousContext { .. }));
    assert!(
        err.to_string().contains("explicit session"),
        "error should guide explicit selection: {err}"
    );
}
