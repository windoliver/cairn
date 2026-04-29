//! Embedded `SQLite` migrations for the Cairn store.
//!
//! Each migration is a hand-written SQL script under `sql/`. Scripts are
//! append-only: never edit a committed file — add a new numbered file.

use rusqlite_migration::{M, Migrations};

const M0001_RECORDS: &str = include_str!("sql/0001_records.sql");
const M0002_WAL: &str = include_str!("sql/0002_wal.sql");
const M0003_REPLAY: &str = include_str!("sql/0003_replay.sql");
const M0004_LOCKS: &str = include_str!("sql/0004_locks.sql");
const M0005_CONSENT: &str = include_str!("sql/0005_consent.sql");
const M0006_DRIFT_HARDENING: &str = include_str!("sql/0006_drift_hardening.sql");
const M0007_CONSENT_EVENT: &str = include_str!("sql/0007_consent_event.sql");
const M0008_CONSENT_EVENT_HARDENING: &str = include_str!("sql/0008_consent_event_hardening.sql");

/// Compile-time manifest of `(migration_id, name, source)` used by the
/// `verify` module to compute and check content hashes.
pub(crate) const MIGRATION_SOURCES: &[(i64, &str, &str)] = &[
    (1, "0001_records", M0001_RECORDS),
    (2, "0002_wal", M0002_WAL),
    (3, "0003_replay", M0003_REPLAY),
    (4, "0004_locks", M0004_LOCKS),
    (5, "0005_consent", M0005_CONSENT),
    (6, "0006_drift_hardening", M0006_DRIFT_HARDENING),
    (7, "0007_consent_event", M0007_CONSENT_EVENT),
    (
        8,
        "0008_consent_event_hardening",
        M0008_CONSENT_EVENT_HARDENING,
    ),
];

/// All migrations, in order. Returns a fresh `Migrations` set on every call
/// so callers may consume it.
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(M0001_RECORDS),
        M::up(M0002_WAL),
        M::up(M0003_REPLAY),
        M::up(M0004_LOCKS),
        M::up(M0005_CONSENT),
        M::up(M0006_DRIFT_HARDENING),
        M::up(M0007_CONSENT_EVENT),
        M::up(M0008_CONSENT_EVENT_HARDENING),
    ])
}
