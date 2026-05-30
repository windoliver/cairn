//! Contract tests for [`SqliteAdminStateStore`] against the real `SQLite` adapter.

#![allow(clippy::unwrap_used, reason = "integration tests may use unwrap")]

use cairn_core::contract::{AdminStateStore, ConnectorStateRow};
use cairn_core::domain::Identity;
use cairn_core::domain::admin::AdminRole;
use cairn_store_sqlite::SqliteAdminStateStore;

fn alice() -> Identity {
    Identity::parse("hmn:alice").expect("test fixture: known-valid identity")
}

fn bootstrap() -> Identity {
    Identity::parse("hmn:bootstrap").expect("test fixture: known-valid identity")
}

fn op() -> Identity {
    Identity::parse("hmn:op").expect("test fixture: known-valid identity")
}

#[test]
fn role_grant_check_revoke_roundtrip() {
    let store = SqliteAdminStateStore::open_in_memory().expect("open in-memory admin store");
    let actor = alice();
    let granter = bootstrap();

    // Initially no operator role.
    assert!(!store.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(!store.has_any_operator().unwrap());

    // Grant and verify.
    store
        .grant_role(&actor, AdminRole::Operator, &granter)
        .unwrap();
    assert!(store.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(store.has_any_operator().unwrap());

    // Idempotent re-grant must not error and must leave role active.
    store
        .grant_role(&actor, AdminRole::Operator, &granter)
        .unwrap();
    assert!(store.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(store.has_any_operator().unwrap());

    // Revoke and verify.
    store.revoke_role(&actor, AdminRole::Operator).unwrap();
    assert!(!store.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(!store.has_any_operator().unwrap());
}

#[test]
fn revoke_no_op_when_not_held() {
    let store = SqliteAdminStateStore::open_in_memory().expect("open in-memory admin store");
    let actor = alice();
    // Revoking a role never held must not error.
    store.revoke_role(&actor, AdminRole::Operator).unwrap();
    assert!(!store.has_role(&actor, AdminRole::Operator).unwrap());
}

#[test]
fn connector_state_upsert_and_list() {
    let store = SqliteAdminStateStore::open_in_memory().expect("open in-memory admin store");
    let by = op();

    // No state row initially.
    assert!(store.get_connector_state("github").unwrap().is_none());
    assert!(store.list_connector_state().unwrap().is_empty());

    // Insert a disabled row with a reason.
    let row = store
        .set_connector_enabled("github", false, &by, Some("rate-limit"))
        .unwrap();
    assert!(!row.enabled);
    assert_eq!(row.connector_name, "github");
    assert_eq!(row.reason.as_deref(), Some("rate-limit"));
    assert_eq!(row.last_changed_by, by);

    // get_connector_state round-trips.
    let fetched = store.get_connector_state("github").unwrap().unwrap();
    assert!(!fetched.enabled);
    assert_eq!(fetched.connector_name, "github");
    assert_eq!(fetched.reason.as_deref(), Some("rate-limit"));

    // list_connector_state returns exactly this row.
    let listed: Vec<ConnectorStateRow> = store.list_connector_state().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].connector_name, "github");
    assert!(!listed[0].enabled);

    // Upsert again to enable, clearing the reason.
    let row2 = store
        .set_connector_enabled("github", true, &by, None)
        .unwrap();
    assert!(row2.enabled);
    assert!(row2.reason.is_none());

    // Verify via get.
    let fetched2 = store.get_connector_state("github").unwrap().unwrap();
    assert!(fetched2.enabled);
    assert!(fetched2.reason.is_none());

    // Still only one row in the list.
    let listed2 = store.list_connector_state().unwrap();
    assert_eq!(listed2.len(), 1);
}

#[test]
fn list_connector_state_is_ordered_by_name() {
    let store = SqliteAdminStateStore::open_in_memory().expect("open in-memory admin store");
    let by = op();

    store
        .set_connector_enabled("zebra", true, &by, None)
        .unwrap();
    store
        .set_connector_enabled("alpha", true, &by, None)
        .unwrap();
    store
        .set_connector_enabled("middle", true, &by, None)
        .unwrap();

    let listed = store.list_connector_state().unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].connector_name, "alpha");
    assert_eq!(listed[1].connector_name, "middle");
    assert_eq!(listed[2].connector_name, "zebra");
}

#[test]
fn multiple_operators_has_any_operator_tracks_all() {
    let store = SqliteAdminStateStore::open_in_memory().expect("open in-memory admin store");
    let alice = alice();
    let granter = bootstrap();
    let bob = Identity::parse("hmn:bob").expect("test fixture: known-valid identity");

    store
        .grant_role(&alice, AdminRole::Operator, &granter)
        .unwrap();
    store
        .grant_role(&bob, AdminRole::Operator, &granter)
        .unwrap();

    // Revoke alice — bob still holds operator.
    store.revoke_role(&alice, AdminRole::Operator).unwrap();
    assert!(!store.has_role(&alice, AdminRole::Operator).unwrap());
    assert!(store.has_role(&bob, AdminRole::Operator).unwrap());
    assert!(store.has_any_operator().unwrap());

    // Revoke bob — no operators remain.
    store.revoke_role(&bob, AdminRole::Operator).unwrap();
    assert!(!store.has_any_operator().unwrap());
}
