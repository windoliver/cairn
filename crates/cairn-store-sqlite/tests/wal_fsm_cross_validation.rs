//! Cross-validation: `cairn_core::wal::fsm` legal-transition predicates
//! must agree with the `SQLite` triggers in
//! `crates/cairn-store-sqlite/src/migrations/sql/0002_wal.sql`.
//!
//! The pure FSM in core is the single source of truth at the API layer;
//! the `SQLite` triggers are the single source of truth at the DB layer.
//! Drift between the two would let invalid transitions slip through one
//! side and be rejected by the other. This proptest seeds an in-memory DB
//! with a minimal `wal_ops` row, then for every (from, to) pair in the
//! 5×5 `OpState` matrix asserts the pure function and the trigger agree.

use cairn_core::wal::{OpState, is_terminal_op, legal_op_transition};
use cairn_store_sqlite::open_in_memory_sync;
use proptest::prelude::*;
use rusqlite::Connection;

fn arb_state() -> impl Strategy<Value = OpState> {
    prop_oneof![
        Just(OpState::Issued),
        Just(OpState::Prepared),
        Just(OpState::Committed),
        Just(OpState::Aborted),
        Just(OpState::Rejected),
    ]
}

fn seed(conn: &Connection, op_id: &str, seq: i64) {
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, issuer, \
          target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
         VALUES (?, ?, 'upsert', 'ISSUED', '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
        rusqlite::params![op_id, seq],
    )
    .expect("seed");
}

fn force_state(conn: &Connection, op_id: &str, target: OpState) -> bool {
    // Forward-walk through any legal path to land at `target`.
    // `OpState` is `#[non_exhaustive]`; the wildcard exists for forward
    // compatibility — if a new variant lands without test coverage here,
    // we want the test to fail loudly rather than silently skip it.
    let path: &[OpState] = match target {
        OpState::Issued => &[],
        OpState::Prepared => &[OpState::Prepared],
        OpState::Committed => &[OpState::Prepared, OpState::Committed],
        OpState::Aborted => &[OpState::Prepared, OpState::Aborted],
        OpState::Rejected => &[OpState::Rejected],
        _ => panic!("unknown OpState variant: {target:?}"),
    };
    for s in path {
        let n = conn
            .execute(
                "UPDATE wal_ops SET state = ? WHERE operation_id = ?",
                rusqlite::params![s.as_str(), op_id],
            )
            .ok();
        if n.is_none() {
            return false;
        }
    }
    true
}

fn current_state(conn: &Connection, op_id: &str) -> String {
    conn.query_row(
        "SELECT state FROM wal_ops WHERE operation_id = ?",
        rusqlite::params![op_id],
        |r| r.get(0),
    )
    .expect("read state")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn pure_fn_agrees_with_sqlite_trigger(from in arb_state(), to in arb_state()) {
        // Filter the case `from == to AND from is terminal`. The pure FSM
        // says `true` for any `f == t` (no transition is happening), but
        // SQLite blocks same-state writes on terminal rows via a separate
        // `wal_ops_terminal_immutable` trigger that is orthogonal to the
        // FSM transition rules we're cross-validating here. The
        // terminal-immutable invariant is exercised in `tests/wal_fsm.rs`.
        prop_assume!(!(from == to && is_terminal_op(from)));

        let conn = open_in_memory_sync().expect("open");
        let op = "op-cross";
        seed(&conn, op, 1);
        prop_assert!(force_state(&conn, op, from), "could not reach {from:?}");
        prop_assert_eq!(current_state(&conn, op), from.as_str().to_owned());

        let pure_says = legal_op_transition(from, to);

        // Same-state writes don't fire the FSM trigger; SQLite returns Ok with
        // 1 row updated. legal_op_transition also returns true for f==t.
        // For different states, the trigger ABORTs on illegal transitions
        // and on terminal-immutable rows (which the pure fn also says
        // false for).
        let result = conn.execute(
            "UPDATE wal_ops SET state = ? WHERE operation_id = ?",
            rusqlite::params![to.as_str(), op],
        );
        let sqlite_says = result.is_ok();

        prop_assert_eq!(
            pure_says, sqlite_says,
            "drift on {:?} -> {:?}: pure={}, sqlite={:?}",
            from, to, pure_says, result.err()
        );
    }
}
