//! Snapshot test for the user-facing `LockError::Held` Display format.
//! Format changes are user-visible (CLI/MCP renderings) — review the snapshot
//! diff carefully before committing.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::time::Duration;

use cairn_store_sqlite::locks::{LockError, LockMode, RetryHint};

#[test]
fn held_display() {
    let e = LockError::Held {
        resource: "vault:abc123".into(),
        mode: LockMode::Exclusive,
        operation: "lint --fix-markdown".into(),
        current_holder: "pid=42-01HQZ7N4VXJ8XK".into(),
        ttl_remaining_ms: 4500,
        since_ms: 500,
        retry: RetryHint::BackoffJitter {
            initial: Duration::from_millis(50),
            max: Duration::from_secs(5),
        },
    };
    insta::assert_snapshot!(format!("{e}"));
}
