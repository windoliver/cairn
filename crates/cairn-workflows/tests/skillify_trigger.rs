#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::job_store::JobStore;
use cairn_workflows::{
    SkillifyEnqueueDecision, SkillifyPayload, SkillifyTrigger, SqliteJobStore, enqueue_skillify,
};
use rusqlite::Connection;

fn store() -> Arc<dyn JobStore> {
    let conn = Connection::open_in_memory().expect("conn");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    Arc::new(SqliteJobStore::new(conn).expect("store"))
}

#[tokio::test]
async fn enqueue_is_idempotent_for_same_key_and_token() {
    let s = store();
    let first: SkillifyEnqueueDecision = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("first");
    let second = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("second");

    assert_eq!(first, second);
}

#[test]
fn payload_round_trips_json() {
    let payload = SkillifyPayload {
        trigger: SkillifyTrigger::DeepDream,
        key: "vault".to_owned(),
        candidate_id: Some("skc_fixture".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    };

    let bytes = payload.to_bytes().expect("encode");
    let back = SkillifyPayload::from_bytes(&bytes).expect("decode");
    assert_eq!(payload, back);
}
