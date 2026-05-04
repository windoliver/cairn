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
const M0007_TOMBSTONE_REASON: &str = include_str!("sql/0007_tombstone_reason.sql");
const M0008_RECORD_EXTENSIONS: &str = include_str!("sql/0008_record_extensions.sql");
const M0009_CONSENT_EVENT: &str = include_str!("sql/0009_consent_event.sql");
const M0010_RANKING_INDEXES: &str = include_str!("sql/0010_ranking_indexes.sql");
const M0011_CONSENT_EVENT_HARDENING: &str = include_str!("sql/0011_consent_event_hardening.sql");
const M0012_FILTER_ALIGNMENT: &str = include_str!("sql/0012_filter_alignment.sql");
const M0013_EDGES_UPDATES_DST_IDX: &str = include_str!("sql/0013_edges_updates_dst_idx.sql");
const M0014_SESSIONS: &str = include_str!("sql/0014_sessions.sql");
const M0015_SESSIONS_UNIQUE_ACTIVE: &str = include_str!("sql/0015_sessions_unique_active.sql");
const M0016_SESSIONS_UNIQUE_ACTIVE_COALESCE: &str =
    include_str!("sql/0016_sessions_unique_active_coalesce.sql");
const M0017_SESSIONS_CLOSE_RELATIVE_PROJECT_ROOT: &str =
    include_str!("sql/0017_sessions_close_relative_project_root.sql");
const M0018_SESSIONS_CANONICALIZE_WINDOWS_PATHS: &str =
    include_str!("sql/0018_sessions_canonicalize_windows_paths.sql");
const M0019_SESSIONS_STRIP_VERBATIM_AND_CASE_FOLD: &str =
    include_str!("sql/0019_sessions_strip_verbatim_and_case_fold.sql");
const M0020_WORKFLOW_JOBS: &str = include_str!("sql/0020_workflow_jobs.sql");
const M0021_CONSENT_KIND_NOT_NULL: &str = include_str!("sql/0021_consent_kind_not_null.sql");
// Renumbered 0020 → 0022 on the bench-branch merge: main shipped its own
// 0020_workflow_jobs first and migration_id 20 was already live in the
// wild. The SQL body is unchanged; only the surrounding `(id, name)` in
// the manifest moved, so on-disk databases that previously applied the
// branch-local 0020 will recognise the renamed row by its sql_hash on
// the next open and stamp the new name.
const M0022_RECORD_VECTORS: &str = include_str!("sql/0022_record_vectors.sql");
const M0030_RECORDS_FTS_WEIGHTED: &str = include_str!("sql/0030_records_fts_weighted.sql");
const M0031_WAL_KIND_WIDENING: &str = include_str!("sql/0031_wal_kind_widening.sql");
const M0032_ENTITY_NODES: &str = include_str!("sql/0032_entity_nodes.sql");
const M0033_ENTITY_EDGES: &str = include_str!("sql/0033_entity_edges.sql");
const M0034_ENTITY_EPISODES: &str = include_str!("sql/0034_entity_episodes.sql");
const M0035_ENTITY_EDGES_NO_OVERLAP_TRIGGER: &str =
    include_str!("sql/0035_entity_edges_no_overlap_trigger.sql");

/// Canonical SQL for migration 0020 (`workflow_jobs`). Re-exported so
/// downstream crates (notably `cairn-workflows`, which hashes the
/// migration source for runtime drift detection) can read it through
/// the package API instead of `include_str!`-ing a sibling crate's
/// source path — the latter breaks `cargo publish` since the sibling
/// is not in the package archive.
pub const WORKFLOW_JOBS_MIGRATION_SQL: &str = M0020_WORKFLOW_JOBS;

/// Compile-time manifest of `(migration_id, name, source)` used by the
/// `verify` module to compute and check content hashes.
pub(crate) const MIGRATION_SOURCES: &[(i64, &str, &str)] = &[
    (1, "0001_records", M0001_RECORDS),
    (2, "0002_wal", M0002_WAL),
    (3, "0003_replay", M0003_REPLAY),
    (4, "0004_locks", M0004_LOCKS),
    (5, "0005_consent", M0005_CONSENT),
    (6, "0006_drift_hardening", M0006_DRIFT_HARDENING),
    (7, "0007_tombstone_reason", M0007_TOMBSTONE_REASON),
    (8, "0008_record_extensions", M0008_RECORD_EXTENSIONS),
    (9, "0009_consent_event", M0009_CONSENT_EVENT),
    (10, "0010_ranking_indexes", M0010_RANKING_INDEXES),
    (
        11,
        "0011_consent_event_hardening",
        M0011_CONSENT_EVENT_HARDENING,
    ),
    (12, "0012_filter_alignment", M0012_FILTER_ALIGNMENT),
    (
        13,
        "0013_edges_updates_dst_idx",
        M0013_EDGES_UPDATES_DST_IDX,
    ),
    (14, "0014_sessions", M0014_SESSIONS),
    (
        15,
        "0015_sessions_unique_active",
        M0015_SESSIONS_UNIQUE_ACTIVE,
    ),
    (
        16,
        "0016_sessions_unique_active_coalesce",
        M0016_SESSIONS_UNIQUE_ACTIVE_COALESCE,
    ),
    (
        17,
        "0017_sessions_close_relative_project_root",
        M0017_SESSIONS_CLOSE_RELATIVE_PROJECT_ROOT,
    ),
    (
        18,
        "0018_sessions_canonicalize_windows_paths",
        M0018_SESSIONS_CANONICALIZE_WINDOWS_PATHS,
    ),
    (
        19,
        "0019_sessions_strip_verbatim_and_case_fold",
        M0019_SESSIONS_STRIP_VERBATIM_AND_CASE_FOLD,
    ),
    (20, "0020_workflow_jobs", M0020_WORKFLOW_JOBS),
    (
        21,
        "0021_consent_kind_not_null",
        M0021_CONSENT_KIND_NOT_NULL,
    ),
    (22, "0022_record_vectors", M0022_RECORD_VECTORS),
    (30, "0030_records_fts_weighted", M0030_RECORDS_FTS_WEIGHTED),
    (31, "0031_wal_kind_widening", M0031_WAL_KIND_WIDENING),
    (32, "0032_entity_nodes", M0032_ENTITY_NODES),
    (33, "0033_entity_edges", M0033_ENTITY_EDGES),
    (34, "0034_entity_episodes", M0034_ENTITY_EPISODES),
    (
        35,
        "0035_entity_edges_no_overlap_trigger",
        M0035_ENTITY_EDGES_NO_OVERLAP_TRIGGER,
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
        M::up(M0007_TOMBSTONE_REASON),
        M::up(M0008_RECORD_EXTENSIONS),
        M::up(M0009_CONSENT_EVENT),
        M::up(M0010_RANKING_INDEXES),
        M::up(M0011_CONSENT_EVENT_HARDENING),
        M::up(M0012_FILTER_ALIGNMENT),
        M::up(M0013_EDGES_UPDATES_DST_IDX),
        M::up(M0014_SESSIONS),
        M::up(M0015_SESSIONS_UNIQUE_ACTIVE),
        M::up(M0016_SESSIONS_UNIQUE_ACTIVE_COALESCE),
        M::up(M0017_SESSIONS_CLOSE_RELATIVE_PROJECT_ROOT),
        M::up(M0018_SESSIONS_CANONICALIZE_WINDOWS_PATHS),
        M::up(M0019_SESSIONS_STRIP_VERBATIM_AND_CASE_FOLD),
        M::up(M0020_WORKFLOW_JOBS),
        M::up(M0021_CONSENT_KIND_NOT_NULL),
        M::up(M0022_RECORD_VECTORS),
        M::up(M0030_RECORDS_FTS_WEIGHTED),
        M::up(M0031_WAL_KIND_WIDENING),
        M::up(M0032_ENTITY_NODES),
        M::up(M0033_ENTITY_EDGES),
        M::up(M0034_ENTITY_EPISODES),
        M::up(M0035_ENTITY_EDGES_NO_OVERLAP_TRIGGER),
    ])
}
