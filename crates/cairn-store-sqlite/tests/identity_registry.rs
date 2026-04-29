//! Integration tests for [`SqliteIdentityRegistry`] (issue #50).
//!
//! Tests in this file exercise the real `SQLite` backend — no mocking.

use cairn_core::contract::identity_registry::IdentityRegistry;
use cairn_store_sqlite::SqliteIdentityRegistry;

// ── C4 · read_vault_meta ──────────────────────────────────────────────────────

/// An empty registry (no first-bind yet) must return `None` from
/// `read_vault_meta`.
#[tokio::test]
async fn read_vault_meta_empty() {
    let r = SqliteIdentityRegistry::open_in_memory().unwrap();
    assert!(r.read_vault_meta().await.unwrap().is_none());
}
