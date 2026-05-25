//! T20d — Rate-limit budget exceeded → `ConnectorError::BudgetExceeded`.
//!
//! Construct a `RateLimit` with capacity 2, charge twice (both succeed),
//! then charge once more to confirm the `BudgetExceeded` error is returned.
//!
//! Issue #130, brief §9.1 source sensors.

#![allow(missing_docs)]

use cairn_connectors_core::{ConnectorError, RateLimit};

#[test]
fn budget_exceeded_returns_typed_error() {
    let rl = RateLimit::per_hour("p1".into(), 2);

    // First two charges must succeed.
    rl.charge("p1", 1).expect("first charge must succeed");
    rl.charge("p1", 1).expect("second charge must succeed");

    // Third charge must fail with BudgetExceeded for scope "p1".
    match rl.charge("p1", 1) {
        Err(ConnectorError::BudgetExceeded { scope }) => {
            assert_eq!(scope, "p1", "BudgetExceeded scope must be \"p1\"");
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
}
