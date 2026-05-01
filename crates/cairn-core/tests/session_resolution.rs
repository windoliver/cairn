//! Session lifecycle pure-resolution tests.

use cairn_core::session::{
    SessionContext, SessionError, SessionIdCandidates, SessionResolutionSource,
};

#[test]
fn explicit_arg_wins_over_harness_and_environment_session_ids() {
    let candidates = SessionIdCandidates {
        explicit_arg: Some("session-cli".to_owned()),
        harness: Some("session-harness".to_owned()),
        environment: Some("session-env".to_owned()),
    };

    let selected = candidates
        .select_direct()
        .expect("candidate validation should pass")
        .expect("direct session id should be selected");

    assert_eq!(selected.session_id, "session-cli");
    assert_eq!(selected.source, SessionResolutionSource::ExplicitArg);
}

#[test]
fn harness_session_id_wins_over_environment_session_id() {
    let candidates = SessionIdCandidates {
        explicit_arg: None,
        harness: Some("session-harness".to_owned()),
        environment: Some("session-env".to_owned()),
    };

    let selected = candidates
        .select_direct()
        .expect("candidate validation should pass")
        .expect("direct session id should be selected");

    assert_eq!(selected.session_id, "session-harness");
    assert_eq!(selected.source, SessionResolutionSource::Harness);
}

#[test]
fn empty_direct_session_id_is_rejected_with_its_source() {
    let candidates = SessionIdCandidates {
        explicit_arg: None,
        harness: None,
        environment: Some(String::new()),
    };

    let err = candidates
        .select_direct()
        .expect_err("blank environment session id must fail closed");

    assert!(matches!(
        err,
        SessionError::InvalidSessionId {
            id_source: SessionResolutionSource::Environment,
            ..
        }
    ));
}

#[test]
fn session_context_requires_user_and_agent_identity() {
    let context = SessionContext {
        user_id: "usr:tafeng".to_owned(),
        agent_id: String::new(),
        vault_id: Some("vault-local".to_owned()),
        project_id: Some("project-a".to_owned()),
        cwd: Some("/work/project-a".to_owned()),
    };

    let err = context
        .validate()
        .expect_err("missing agent identity makes auto-discovery ambiguous");

    assert!(matches!(
        err,
        SessionError::AmbiguousContext { ref reason }
            if reason.contains("agent_id")
    ));
}
