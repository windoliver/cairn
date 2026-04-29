//! WAL state-machine property tests.
//!
//! Generates random transition sequences and asserts `SQLite` enforces
//! the §5.6 FSM exactly: ISSUED -> {PREPARED, REJECTED};
//! PREPARED -> {COMMITTED, ABORTED}; everything else aborts.

use cairn_store_sqlite::open_in_memory;
use proptest::prelude::*;
use rusqlite::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Issued,
    Prepared,
    Committed,
    Aborted,
    Rejected,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Issued => "ISSUED",
            State::Prepared => "PREPARED",
            State::Committed => "COMMITTED",
            State::Aborted => "ABORTED",
            State::Rejected => "REJECTED",
        }
    }
}

fn allowed(from: State, to: State) -> bool {
    matches!(
        (from, to),
        (State::Issued, State::Prepared | State::Rejected)
            | (State::Prepared, State::Committed | State::Aborted)
    )
}

fn arb_state() -> impl Strategy<Value = State> {
    prop_oneof![
        Just(State::Issued),
        Just(State::Prepared),
        Just(State::Committed),
        Just(State::Aborted),
        Just(State::Rejected),
    ]
}

fn seed_issued(conn: &Connection, op_id: &str, seq: i64) {
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, issuer, \
          target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
         VALUES (?, ?, 'upsert', 'ISSUED', '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
        rusqlite::params![op_id, seq],
    )
    .expect("seed wal_ops");
}

fn try_set_state(conn: &Connection, op_id: &str, target: State) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE wal_ops SET state = ? WHERE operation_id = ?",
        rusqlite::params![target.as_str(), op_id],
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn fsm_enforces_section_5_6(targets in proptest::collection::vec(arb_state(), 1..6)) {
        let conn = open_in_memory().expect("open");
        let op_id = "op-fsm";
        seed_issued(&conn, op_id, 1);

        let mut current = State::Issued;
        for target in targets {
            let result = try_set_state(&conn, op_id, target);
            let is_terminal = matches!(current, State::Committed | State::Aborted | State::Rejected);

            if target == current {
                // No-op UPDATE: trigger only fires when state actually changes,
                // so terminal-state-immutable still allows same-value writes.
                // We don't assert on the result here.
                continue;
            }

            if is_terminal {
                prop_assert!(result.is_err(),
                    "terminal {:?} -> {:?} must fail", current, target);
            } else if allowed(current, target) {
                prop_assert!(result.is_ok(),
                    "allowed {:?} -> {:?} must succeed: {:?}", current, target, result);
                current = target;
            } else {
                prop_assert!(result.is_err(),
                    "forbidden {:?} -> {:?} must fail", current, target);
            }
        }
    }
}

#[test]
fn terminal_states_reject_any_transition() {
    let conn = open_in_memory().expect("open");
    seed_issued(&conn, "op-term", 1);
    try_set_state(&conn, "op-term", State::Prepared).expect("ISSUED->PREPARED");
    try_set_state(&conn, "op-term", State::Committed).expect("PREPARED->COMMITTED");

    for next in [
        State::Issued,
        State::Prepared,
        State::Aborted,
        State::Rejected,
    ] {
        let err = try_set_state(&conn, "op-term", next).unwrap_err();
        assert!(
            format!("{err}").contains("terminal-state") || format!("{err}").contains("FSM"),
            "expected terminal/FSM error for COMMITTED -> {next:?}, got: {err}"
        );
    }
}
