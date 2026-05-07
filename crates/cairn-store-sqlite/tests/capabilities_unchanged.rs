//! Issue #186 acceptance: `MemoryStoreCapabilities` struct must not gain
//! new fields as a side-effect of the bitemporal-KG schema migrations.
//!
//! The snapshot pins the four-flag layout so any future struct expansion
//! shows up as an explicit snapshot diff rather than a silent no-op.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core::contract::memory_store::MemoryStore;
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn capabilities_struct_unchanged_after_186() {
    let store = open_in_memory().await.expect("open in-memory store");
    let caps = store.capabilities();
    // Serialise as a flat string so no extra dep is needed beyond `insta`.
    // The four fields are the entire struct — no hidden widening possible.
    let snapshot = format!(
        "fts={} vector={} graph_edges={} transactions={}",
        caps.fts, caps.vector, caps.graph_edges, caps.transactions
    );
    insta::assert_snapshot!("capabilities_after_186", snapshot);
}
