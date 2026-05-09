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
// Renumbered 0022 → 0023 on rebase since main shipped 0022_record_vectors
// first. SQL body unchanged.
const M0023_TRACE_LINKS: &str = include_str!("sql/0023_trace_links.sql");
const M0030_RECORDS_FTS_WEIGHTED: &str = include_str!("sql/0030_records_fts_weighted.sql");
// Issue #253 (consent receipt timeline) — main shipped 0031..0040.
const M0031_RECORDS_CONSENT_MODEL: &str = include_str!("sql/0031_records_consent_model.sql");
const M0032_CONSENT_TIMELINE: &str = include_str!("sql/0032_consent_timeline.sql");
const M0033_CONSENT_TIMELINE_GRANT_IMMUTABLE: &str =
    include_str!("sql/0033_consent_timeline_grant_immutable.sql");
const M0034_CONSENT_TIMELINE_LIFECYCLE: &str =
    include_str!("sql/0034_consent_timeline_lifecycle.sql");
const M0035_CONSENT_TIMELINE_LIFECYCLE_TIGHTEN: &str =
    include_str!("sql/0035_consent_timeline_lifecycle_tighten.sql");
const M0036_CONSENT_TIMELINE_CANONICAL_UTC: &str =
    include_str!("sql/0036_consent_timeline_canonical_utc.sql");
const M0037_CONSENT_TIMELINE_CANONICAL_NANOS: &str =
    include_str!("sql/0037_consent_timeline_canonical_nanos.sql");
const M0038_CONSENT_TIMELINE_ASSERT_CANONICAL_NANOS: &str =
    include_str!("sql/0038_consent_timeline_assert_canonical_nanos.sql");
const M0039_CONSENT_TIMELINE_AUDIT_LEGACY_INVARIANTS: &str =
    include_str!("sql/0039_consent_timeline_audit_legacy_invariants.sql");
const M0040_CONSENT_TIMELINE_SCOPE_CANONICAL: &str =
    include_str!("sql/0040_consent_timeline_scope_canonical.sql");
// Issue #186 (bitemporal KG substrate) — renumbered onto 0041..0045
// during the rebase onto main, which had already taken 0031..0040 for
// the consent receipt timeline.
const M0041_WAL_KIND_WIDENING: &str = include_str!("sql/0041_wal_kind_widening.sql");
const M0042_ENTITY_NODES: &str = include_str!("sql/0042_entity_nodes.sql");
const M0043_ENTITY_EDGES: &str = include_str!("sql/0043_entity_edges.sql");
const M0044_ENTITY_EPISODES: &str = include_str!("sql/0044_entity_episodes.sql");
const M0045_ENTITY_EDGES_NO_OVERLAP_TRIGGER: &str =
    include_str!("sql/0045_entity_edges_no_overlap_trigger.sql");
// Issue #258 — renumbered from 0041 to 0046 during rebase, since
// #186 (KG substrate) had already taken 0041..0045 on main.
const M0046_RECORDS_SCHEMA_VERSION: &str = include_str!("sql/0046_records_schema_version.sql");
// Issue #254 (lint --fix-markdown) — renumbered from 0031 to 0047
// during rebase, since #253 (consent timeline) had already taken
// 0031..0040 and #186 (KG substrate) + #258 (schema_version) had
// taken 0041..0046 on main.
const M0047_WAL_LINT_REPAIR: &str = include_str!("sql/0047_wal_lint_repair.sql");
// Issue #267 — renumbered from 0046 to 0048 during rebase, since main
// had already taken 0046..0047.
const M0048_CONSENT_JOURNAL_REPAIR_AUDIT: &str =
    include_str!("sql/0048_consent_journal_repair_audit.sql");
// Issue #52 — renumbered from 0046 → 0047 → 0048 → 0049 across three
// main merges as #258 / #254 / #267 each took the next slot.
const M0049_REPLAY_CHALLENGE_MODE: &str = include_str!("sql/0049_replay_challenge_mode.sql");
// Issue #56 — epoch fencing + daemon_incarnation wiring (brief §5.6).
const M0050_LOCKS_V2: &str = include_str!("sql/0050_locks_v2.sql");
// Issue #56 round-7 fix — per-acquisition `acquisition_ulid` column.
const M0051_LOCK_ACQUISITION_ULID: &str = include_str!("sql/0051_lock_acquisition_ulid.sql");
// Issue #56 round-10 fix — UNIQUE constraint + sentinel-rejection trigger.
const M0052_LOCK_ACQUISITION_ULID_UNIQUE: &str =
    include_str!("sql/0052_lock_acquisition_ulid_unique.sql");
// Issue #57 — durable body-bearing WAL payloads for upsert/expire recovery.
const M0053_WAL_PAYLOADS: &str = include_str!("sql/0053_wal_payloads.sql");
// Issue #57 round-2 — internal marker for pre-activation COW rows.
const M0054_RECORDS_COW_STAGING: &str = include_str!("sql/0054_records_cow_staging.sql");

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
    (23, "0023_trace_links", M0023_TRACE_LINKS),
    (30, "0030_records_fts_weighted", M0030_RECORDS_FTS_WEIGHTED),
    (
        31,
        "0031_records_consent_model",
        M0031_RECORDS_CONSENT_MODEL,
    ),
    (32, "0032_consent_timeline", M0032_CONSENT_TIMELINE),
    (
        33,
        "0033_consent_timeline_grant_immutable",
        M0033_CONSENT_TIMELINE_GRANT_IMMUTABLE,
    ),
    (
        34,
        "0034_consent_timeline_lifecycle",
        M0034_CONSENT_TIMELINE_LIFECYCLE,
    ),
    (
        35,
        "0035_consent_timeline_lifecycle_tighten",
        M0035_CONSENT_TIMELINE_LIFECYCLE_TIGHTEN,
    ),
    (
        36,
        "0036_consent_timeline_canonical_utc",
        M0036_CONSENT_TIMELINE_CANONICAL_UTC,
    ),
    (
        37,
        "0037_consent_timeline_canonical_nanos",
        M0037_CONSENT_TIMELINE_CANONICAL_NANOS,
    ),
    (
        38,
        "0038_consent_timeline_assert_canonical_nanos",
        M0038_CONSENT_TIMELINE_ASSERT_CANONICAL_NANOS,
    ),
    (
        39,
        "0039_consent_timeline_audit_legacy_invariants",
        M0039_CONSENT_TIMELINE_AUDIT_LEGACY_INVARIANTS,
    ),
    (
        40,
        "0040_consent_timeline_scope_canonical",
        M0040_CONSENT_TIMELINE_SCOPE_CANONICAL,
    ),
    (41, "0041_wal_kind_widening", M0041_WAL_KIND_WIDENING),
    (42, "0042_entity_nodes", M0042_ENTITY_NODES),
    (43, "0043_entity_edges", M0043_ENTITY_EDGES),
    (44, "0044_entity_episodes", M0044_ENTITY_EPISODES),
    (
        45,
        "0045_entity_edges_no_overlap_trigger",
        M0045_ENTITY_EDGES_NO_OVERLAP_TRIGGER,
    ),
    (
        46,
        "0046_records_schema_version",
        M0046_RECORDS_SCHEMA_VERSION,
    ),
    (47, "0047_wal_lint_repair", M0047_WAL_LINT_REPAIR),
    (
        48,
        "0048_consent_journal_repair_audit",
        M0048_CONSENT_JOURNAL_REPAIR_AUDIT,
    ),
    (
        49,
        "0049_replay_challenge_mode",
        M0049_REPLAY_CHALLENGE_MODE,
    ),
    (50, "0050_locks_v2", M0050_LOCKS_V2),
    (
        51,
        "0051_lock_acquisition_ulid",
        M0051_LOCK_ACQUISITION_ULID,
    ),
    (
        52,
        "0052_lock_acquisition_ulid_unique",
        M0052_LOCK_ACQUISITION_ULID_UNIQUE,
    ),
    (53, "0053_wal_payloads", M0053_WAL_PAYLOADS),
    (54, "0054_records_cow_staging", M0054_RECORDS_COW_STAGING),
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
        M::up(M0023_TRACE_LINKS),
        M::up(M0030_RECORDS_FTS_WEIGHTED),
        M::up(M0031_RECORDS_CONSENT_MODEL),
        M::up(M0032_CONSENT_TIMELINE),
        M::up(M0033_CONSENT_TIMELINE_GRANT_IMMUTABLE),
        M::up(M0034_CONSENT_TIMELINE_LIFECYCLE),
        M::up(M0035_CONSENT_TIMELINE_LIFECYCLE_TIGHTEN),
        M::up(M0036_CONSENT_TIMELINE_CANONICAL_UTC),
        M::up(M0037_CONSENT_TIMELINE_CANONICAL_NANOS),
        M::up(M0038_CONSENT_TIMELINE_ASSERT_CANONICAL_NANOS),
        M::up(M0039_CONSENT_TIMELINE_AUDIT_LEGACY_INVARIANTS),
        M::up(M0040_CONSENT_TIMELINE_SCOPE_CANONICAL),
        M::up(M0041_WAL_KIND_WIDENING),
        M::up(M0042_ENTITY_NODES),
        M::up(M0043_ENTITY_EDGES),
        M::up(M0044_ENTITY_EPISODES),
        M::up(M0045_ENTITY_EDGES_NO_OVERLAP_TRIGGER),
        M::up(M0046_RECORDS_SCHEMA_VERSION),
        M::up(M0047_WAL_LINT_REPAIR),
        M::up(M0048_CONSENT_JOURNAL_REPAIR_AUDIT),
        M::up(M0049_REPLAY_CHALLENGE_MODE),
        M::up(M0050_LOCKS_V2),
        M::up(M0051_LOCK_ACQUISITION_ULID),
        M::up(M0052_LOCK_ACQUISITION_ULID_UNIQUE),
        M::up(M0053_WAL_PAYLOADS),
        M::up(M0054_RECORDS_COW_STAGING),
    ])
}
