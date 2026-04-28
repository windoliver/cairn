//! Generic `MemoryStore` conformance suite (brief §4 row 1, plan
//! issue-46 step 4.1).
//!
//! Future store implementations (e.g. `cairn-store-nexus`) call
//! [`run_conformance`] with a factory closure that yields a fresh store
//! instance. The suite re-runs the lifecycle checks the `SQLite` store
//! crate proves out in `crates/cairn-store-sqlite/tests/`.
//!
//! At issue-46 the suite is intentionally a stub: the `SQLite`-backed
//! integration tests are the load-bearing coverage. Sibling stores can
//! plug in later by porting those test bodies into helpers below; the
//! signature is stable so the call site in a new crate compiles today.

use std::future::Future;
use std::pin::Pin;

use cairn_core::contract::MemoryStore;
use cairn_core::contract::memory_store::apply::MemoryStoreApply;

/// Boxed future returning a freshly-opened store instance.
pub type StoreFactory<S> = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = S> + Send>> + Send>;

/// Run the generic conformance suite against the store produced by
/// `make_store`.
///
/// Each helper exercises one slice of the lifecycle. Helpers are sync
/// stubs at issue-46 — the `SQLite` integration tests carry the real
/// coverage. Promote bodies here when a second store lands.
pub async fn run_conformance<S>(make_store: StoreFactory<S>)
where
    S: MemoryStore + MemoryStoreApply + 'static,
{
    let store = make_store().await;
    crud_roundtrip(&store);
    cow_versioning(&store);
    tombstone_preserves_history(&store);
    expire_active(&store);
    purge_audit(&store);
}

fn crud_roundtrip<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. See `crates/cairn-store-sqlite/tests/crud_roundtrip.rs` for
    // the canonical body. Promote here when a second store implements
    // the contract.
}

fn cow_versioning<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. See `crates/cairn-store-sqlite/tests/cow_versioning.rs`.
}

fn tombstone_preserves_history<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. See
    // `crates/cairn-store-sqlite/tests/tombstone_preserves_history.rs`.
}

fn expire_active<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. See `crates/cairn-store-sqlite/tests/expire_active.rs`.
}

fn purge_audit<S: MemoryStore + MemoryStoreApply>(_store: &S) {
    // Stub. See `crates/cairn-store-sqlite/tests/purge_audit_marker.rs`.
}
