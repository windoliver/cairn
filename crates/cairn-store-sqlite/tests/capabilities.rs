// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `MemoryStore::capabilities` advertises the static feature matrix the
//! verb layer dispatches on. P0 ships with FTS5 + graph edges + ACID
//! transactions; vector ANN is deferred to issue #48.

use cairn_core::contract::MemoryStore;
use cairn_core::contract::version::ContractVersion;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

#[tokio::test]
async fn caps_advertise_p0_feature_matrix() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let caps = store.capabilities();
    assert!(caps.fts, "P0 must advertise fts");
    assert!(!caps.vector, "vector deferred to #48");
    assert!(caps.graph_edges, "P0 must advertise graph_edges");
    assert!(caps.transactions, "P0 must advertise transactions");
}

#[tokio::test]
async fn supported_contract_versions_accepts_v0_2() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let range = store.supported_contract_versions();
    assert!(range.accepts(ContractVersion::new(0, 2, 0)));
    assert!(!range.accepts(ContractVersion::new(0, 1, 0)));
    assert!(!range.accepts(ContractVersion::new(0, 3, 0)));
}
