//! Session source normalization tests for CLI/adapters.

use std::collections::BTreeMap;
use std::fs;

use cairn_cli::session::{
    CAIRN_SESSION_ENV, discover_project_context, session_candidates_from_env,
};

#[test]
fn session_candidates_use_cli_arg_before_environment() {
    let mut env = BTreeMap::new();
    env.insert(CAIRN_SESSION_ENV.to_owned(), "session-env".to_owned());

    let candidates = session_candidates_from_env(Some("session-cli".to_owned()), None, &env);
    let selected = candidates
        .select_direct()
        .expect("candidate validation")
        .expect("direct candidate");

    assert_eq!(selected.session_id, "session-cli");
}

#[test]
fn session_candidates_read_cairn_session_id_environment() {
    let mut env = BTreeMap::new();
    env.insert(CAIRN_SESSION_ENV.to_owned(), "session-env".to_owned());

    let candidates = session_candidates_from_env(None, None, &env);
    let selected = candidates
        .select_direct()
        .expect("candidate validation")
        .expect("direct candidate");

    assert_eq!(selected.session_id, "session-env");
}

#[test]
fn project_discovery_uses_nearest_project_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    let nested = project.join("src/bin");
    fs::create_dir_all(&nested).expect("nested dirs");
    fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("project marker");

    let context = discover_project_context(&nested).expect("project discovery");

    assert_eq!(
        context.project_id,
        fs::canonicalize(&project)
            .expect("canonical project")
            .display()
            .to_string()
    );
    assert_eq!(
        context.cwd,
        fs::canonicalize(&nested)
            .expect("canonical cwd")
            .display()
            .to_string()
    );
}

#[test]
fn project_discovery_uses_cwd_when_no_marker_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("scratch");
    fs::create_dir_all(&cwd).expect("cwd");

    let context = discover_project_context(&cwd).expect("project discovery");
    let canonical = fs::canonicalize(&cwd).expect("canonical cwd");

    assert_eq!(context.project_id, canonical.display().to_string());
    assert_eq!(context.cwd, canonical.display().to_string());
}
