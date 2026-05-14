//! SQLite-backed [`JobStore`] for the default tokio orchestrator.
//!
//! Owns a single `rusqlite::Connection` opened against `.cairn/cairn.db`.
//! Migration 0020 (in `cairn-store-sqlite`) provisions the
//! `workflow_jobs` table. We only read/write that table — schema
//! ownership stays with the store crate, runtime ownership stays here.
//!
//! `rusqlite::Connection` is `Send` but `!Sync`, so we wrap it in
//! `std::sync::Mutex` and execute every call inside
//! `tokio::task::spawn_blocking` to keep the async runtime unblocked.
//! Mutex guards never span an `.await`.

use std::sync::{Arc, Mutex};

use cairn_core::contract::{
    EnqueueRequest, FailDisposition, JobId, JobKind, JobStore, JobStoreError, LeaseToken,
    LeasedJob, RetryPolicy,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// Canonical SQL for migration 0020, re-exported from
/// `cairn-store-sqlite` so the constructor can hash it against the
/// `schema_migrations.sql_hash` row for `migration_id = 20` and
/// detect tampering with the row that birthed our table. Pulling the
/// constant through the package API instead of `include_str!`-ing a
/// sibling crate's source keeps `cairn-workflows` self-contained when
/// packaged for publish. Future migrations (12, 13, …) add rows but
/// do not change this one, so the check is forward-compatible.
const MIGRATION_0020_SQL: &str = cairn_store_sqlite::migrations::WORKFLOW_JOBS_MIGRATION_SQL;

/// Multiplier applied to `max_attempts` to derive a hard ceiling on
/// raw lease deliveries (started or not). Prevents pre-heartbeat crash
/// loops from looping forever while keeping `max_attempts` as a clean
/// execution-count contract. Tuned well above any plausible execution
/// budget — see `RetryPolicy::max_attempts` docs.
const POISON_DELIVERY_MULTIPLIER: i64 = 5;

/// Schema objects from migration 0020 that the runtime relies on. The
/// constructor probes for each so a partially-migrated DB fails fast
/// instead of running with weakened guarantees.
const REQUIRED_SCHEMA: &[(&str, &str)] = &[
    ("table", "workflow_jobs"),
    ("index", "workflow_jobs_ready_idx"),
    ("index", "workflow_jobs_queued_queue_key_idx"),
    ("index", "workflow_jobs_lease_expiry_idx"),
    ("index", "workflow_jobs_queue_key_leased_uniq"),
    ("index", "workflow_jobs_dedupe_uniq"),
    ("trigger", "workflow_jobs_identity_immutable"),
    ("trigger", "workflow_jobs_terminal_absorbing"),
    ("trigger", "workflow_jobs_state_transition"),
    ("trigger", "workflow_jobs_no_delete"),
];

/// A [`JobStore`] backed by a single `SQLite` connection.
pub struct SqliteJobStore {
    conn: Arc<Mutex<Connection>>,
}

/// Errors from [`SqliteJobStore::new`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SqliteJobStoreInitError {
    /// The connection points at a database that has not had migration
    /// 0020 applied — one or more required schema objects (table,
    /// index, or trigger) are missing.
    #[error(
        "workflow_jobs schema incomplete (missing {kind} {name}) — run migrations via cairn-store-sqlite first"
    )]
    MigrationMissing {
        /// Object kind (`table` | `index` | `trigger`).
        kind: &'static str,
        /// Object name.
        name: &'static str,
    },
    /// The `schema_migrations` row for migration 0020 has a `sql_hash`
    /// that doesn't match the canonical migration source compiled into
    /// this binary — the on-disk schema has drifted (e.g. a trigger or
    /// index was dropped and recreated with weaker predicates).
    #[error(
        "workflow_jobs schema drift: migration 0020 sql_hash mismatch \
         (on-disk {on_disk}, expected {expected}) — re-run migrations against a fresh DB"
    )]
    SchemaDrift {
        /// Hash recorded in `schema_migrations`.
        on_disk: String,
        /// Hash computed from the binary's compiled-in migration source.
        expected: String,
    },
    /// Backend error during probe / `PRAGMA` setup.
    #[error("sqlite probe failed: {0}")]
    Backend(String),
}

impl SqliteJobStore {
    /// Wrap an opened connection.
    ///
    /// Probes that every required `workflow_jobs` schema object (the
    /// table, its indexes, and its triggers from migration 0020) is
    /// present by name, and that the runtime invariants this binary
    /// depends on still reject a known-invalid mutation. Catches the
    /// common failure modes — wrong DB, partially migrated DB, CHECK
    /// relaxed to allow `state='queued'` with a lease owner — without
    /// hard-coding an exact DDL snapshot (which would brick the
    /// orchestrator on legitimate future migrations that evolve this
    /// table).
    ///
    /// Full schema-fingerprint verification is the canonical opener's
    /// job (`cairn-store-sqlite::open_sync` runs
    /// `verify_schema_fingerprint` against the head schema). Callers
    /// should obtain `Connection` from there for end-to-end drift
    /// detection.
    ///
    /// Also applies a 5 s `PRAGMA busy_timeout` so brief write-lock
    /// collisions retry internally.
    ///
    /// # Errors
    ///
    /// [`SqliteJobStoreInitError::MigrationMissing`] when any required
    /// object is absent;
    /// [`SqliteJobStoreInitError::SchemaDrift`] when a runtime
    /// invariant probe fails (e.g. queued-state CHECK no longer
    /// rejects a row with a lease owner);
    /// [`SqliteJobStoreInitError::Backend`] for any other `SQLite`
    /// failure during probe.
    pub fn new(conn: Connection) -> Result<Self, SqliteJobStoreInitError> {
        // 5 s busy_timeout — long enough that a brief SQLite write
        // lock collision retries internally instead of bubbling up.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| SqliteJobStoreInitError::Backend(e.to_string()))?;
        for &(kind, name) in REQUIRED_SCHEMA {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = ? AND name = ?",
                    params![kind, name],
                    |r| r.get(0),
                )
                .map_err(|e| SqliteJobStoreInitError::Backend(e.to_string()))?;
            if exists == 0 {
                return Err(SqliteJobStoreInitError::MigrationMissing { kind, name });
            }
        }

        // Migration-history check: the canonical opener stamps a
        // SHA256 of the migration SQL into `schema_migrations.sql_hash`.
        // Compare migration 20's stored hash against the source we
        // were compiled against; a mismatch means someone tampered
        // with the row that birthed `workflow_jobs`. This is the
        // verify_migration_history equivalent for our one migration.
        // Future migrations 12+ add new rows; they do not alter row
        // 11, so this check is forward-compatible.
        let stamp_decision = verify_migration_20_hash(&conn)?;

        // Runtime-invariant probe: the queued-state CHECK is the
        // central correctness anchor (no queued row may carry a lease
        // owner, etc.). Try to insert a deliberately-invalid queued
        // row inside a savepoint; SQLite must reject. If it doesn't,
        // a `CHECK` was relaxed and the runtime is no longer safe.
        // The savepoint is always rolled back so the DB is unchanged.
        verify_runtime_invariants(&conn)?;

        // Only now — after probes prove the live schema enforces every
        // invariant the canonical migration installs — do we stamp a
        // previously-unstamped `sql_hash` row. Stamping before probes
        // would let a hand-built schema that passes the name check be
        // permanently blessed, even if a probe failure rolled the
        // constructor back: a subsequent startup would see a stamped
        // hash and skip this branch entirely.
        if let StampDecision::Stamp { expected } = stamp_decision {
            // Conditional update: only stamp when the row is still
            // empty. Two processes opening the same freshly-migrated
            // DB can race here — both saw `sql_hash = ''`, both passed
            // probes, both want to stamp. Without `WHERE sql_hash = ''`
            // the loser hits the schema_migrations_immutable trigger
            // (which permits exactly one '' -> hash transition) and
            // fails startup. With it, the loser writes zero rows and
            // we re-verify against whatever the winner stamped.
            let updated = conn
                .execute(
                    "UPDATE schema_migrations SET sql_hash = ? \
                       WHERE migration_id = 20 AND sql_hash = ''",
                    rusqlite::params![&expected],
                )
                .map_err(|e| SqliteJobStoreInitError::Backend(e.to_string()))?;
            if updated == 0 {
                // Lost the race — confirm the winner stamped the
                // canonical hash. If they stamped something else,
                // surface as SchemaDrift just like a normal startup.
                match verify_migration_20_hash(&conn)? {
                    StampDecision::AlreadyStamped => {}
                    StampDecision::Stamp { .. } => {
                        return Err(SqliteJobStoreInitError::Backend(
                            "schema_migrations row 11 reverted to empty after concurrent stamp"
                                .to_string(),
                        ));
                    }
                }
            }
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

/// SHA256 of the embedded migration SQL, lowercase hex. Stamped by
/// the canonical opener into `schema_migrations.sql_hash` and
/// re-computed here for comparison.
fn compiled_migration_hash() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(MIGRATION_0020_SQL.as_bytes());
    let bytes = h.finalize();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

enum StampDecision {
    /// Hash row already matches the compiled source — nothing to do.
    AlreadyStamped,
    /// Hash row is empty (migration 0020 inserted '' and the canonical
    /// opener hasn't stamped yet). The constructor must commit this
    /// hash *only after* runtime-invariant probes pass.
    Stamp { expected: String },
}

fn verify_migration_20_hash(conn: &Connection) -> Result<StampDecision, SqliteJobStoreInitError> {
    let on_disk: Option<String> = conn
        .query_row(
            "SELECT sql_hash FROM schema_migrations WHERE migration_id = 20",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| SqliteJobStoreInitError::Backend(e.to_string()))?;
    let expected = compiled_migration_hash();
    match on_disk {
        None => Err(SqliteJobStoreInitError::MigrationMissing {
            kind: "row",
            name: "schema_migrations[migration_id=11]",
        }),
        Some(h) if h.is_empty() => Ok(StampDecision::Stamp { expected }),
        Some(h) if h == expected => Ok(StampDecision::AlreadyStamped),
        Some(h) => Err(SqliteJobStoreInitError::SchemaDrift {
            on_disk: h,
            expected,
        }),
    }
}

/// Categorize the rusqlite error a probe statement returned.
enum ProbeOutcome {
    /// Statement succeeded — the invariant we wanted enforced was NOT
    /// rejected, which means the schema is drifted.
    Accepted,
    /// Statement was rejected by a CHECK / UNIQUE / trigger ABORT — the
    /// invariant holds.
    RejectedByConstraint,
    /// Statement failed for some other reason (locked DB, missing
    /// column, type mismatch, …). This is *not* proof the invariant
    /// holds — escalate to the caller as Backend.
    BackendError(String),
}

fn classify(result: rusqlite::Result<usize>) -> ProbeOutcome {
    match result {
        Ok(_) => ProbeOutcome::Accepted,
        Err(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            ProbeOutcome::RejectedByConstraint
        }
        Err(e) => ProbeOutcome::BackendError(e.to_string()),
    }
}

fn drift(invariant: &'static str) -> SqliteJobStoreInitError {
    SqliteJobStoreInitError::SchemaDrift {
        on_disk: format!("invariant relaxed: {invariant}"),
        expected: format!("schema must enforce: {invariant}"),
    }
}

/// SQL skeleton for inserting a queued probe row. Caller supplies the
/// probe-unique values via positional parameters in the order:
/// (`job_id`, kind, `queue_key`, `dedupe_key`, `lease_owner`, `lease_started`).
/// All other columns take fixed safe defaults.
const PROBE_INSERT_QUEUED: &str = "INSERT INTO workflow_jobs \
    (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
     base_backoff_ms, backoff_multiplier, max_backoff_ms, \
     queue_key, dedupe_key, next_run_at, \
     lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
     enqueued_at, updated_at) \
 VALUES (?, ?, x'', 'queued', 0, 0, 1, 0, 1, 0, ?, ?, 0, ?, NULL, ?, NULL, NULL, 0, 0)";

/// Generate a fresh, collision-resistant ID for use inside a probe.
/// ULIDs are 26-char Crockford-base32; suffixed with a tag so the
/// origin is obvious in the unlikely event a probe row leaks.
fn probe_id(tag: &str) -> String {
    format!("__cairn_probe_{tag}_{}__", ulid::Ulid::new())
}

/// Probe each runtime invariant `workflow_jobs` must enforce. Every
/// probe is wrapped in a SAVEPOINT and rolled back, so the database
/// is left unchanged regardless of outcome. A probe that succeeds, or
/// fails with anything other than a constraint violation, is treated
/// as drift / backend failure rather than as silent passing. Probe
/// IDs are freshly generated ULIDs so they cannot collide with
/// caller-chosen job IDs persisted on disk.
#[allow(clippy::too_many_lines)] // Each probe is a small, sequential block; splitting hurts readability.
fn verify_runtime_invariants(conn: &Connection) -> Result<(), SqliteJobStoreInitError> {
    use rusqlite::types::Null;

    // Probe 1: queued-state shape CHECK — a queued row may not carry
    // a lease_owner. Expect: rejected by CHECK.
    {
        let id = probe_id("q");
        let kind = probe_id("k");
        let setup: Vec<Box<dyn ProbeStmt>> = vec![Box::new(ParamStmt::new(
            PROBE_INSERT_QUEUED,
            vec![
                Box::new(id),
                Box::new(kind),
                Box::new(Null),
                Box::new(Null),
                Box::new("probe-owner".to_string()),
                Box::new(Null),
            ],
        ))];
        run_probes(
            conn,
            &setup,
            ProbeExpect::ConstraintRejected,
            "queued rows must have NULL lease_owner",
        )?;
    }

    // Probe 2: queue_key partial-unique index over `state = 'leased'`
    // — at most one leased row per queue_key. Multiple queued rows
    // with the same key are now allowed (per-key serialization
    // happens at lease time, not enqueue time); the unique
    // constraint only fires once two rows reach `leased`.
    {
        let id1 = probe_id("qkl1");
        let id2 = probe_id("qkl2");
        let kind = probe_id("k");
        let qk = probe_id("queue");
        let leased_insert = "INSERT INTO workflow_jobs \
                (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
                 base_backoff_ms, backoff_multiplier, max_backoff_ms, \
                 queue_key, dedupe_key, next_run_at, \
                 lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
                 enqueued_at, updated_at) \
             VALUES (?, ?, x'', 'leased', 1, 1, 1, 0, 1, 0, ?, NULL, 0, \
                     ?, ?, 0, 1, NULL, 0, 0)";
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                leased_insert,
                vec![
                    Box::new(id1),
                    Box::new(kind.clone()),
                    Box::new(qk.clone()),
                    Box::new(probe_id("owner")),
                    Box::new(probe_id("nonce")),
                ],
            )),
            Box::new(ParamStmt::new(
                leased_insert,
                vec![
                    Box::new(id2),
                    Box::new(kind),
                    Box::new(qk),
                    Box::new(probe_id("owner")),
                    Box::new(probe_id("nonce")),
                ],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "at most one leased row per queue_key",
        )?;
    }

    // Probe 2b: queue_key backlog coexistence. Two queued rows with the
    // same queue_key must coexist — per-key serialization happens at
    // lease time. A drifted index broadened to all states (e.g.
    // `UNIQUE(queue_key) WHERE queue_key IS NOT NULL`) would still
    // pass probe 2 but break legitimate enqueue. Positive probe.
    {
        let id1 = probe_id("qkq1");
        let id2 = probe_id("qkq2");
        let kind = probe_id("k");
        let qk = probe_id("queue");
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id1),
                    Box::new(kind.clone()),
                    Box::new(qk.clone()),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id2),
                    Box::new(kind),
                    Box::new(qk),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::Accepted,
            "many queued rows may share a queue_key",
        )?;
    }

    // Probe 3: (kind, dedupe_key) partial-unique index — two active
    // rows with the same (kind, dedupe_key) must not coexist.
    {
        let id1 = probe_id("dk1");
        let id2 = probe_id("dk2");
        let kind = probe_id("k");
        let dk = probe_id("dedupe");
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id1),
                    Box::new(kind.clone()),
                    Box::new(Null),
                    Box::new(dk.clone()),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id2),
                    Box::new(kind),
                    Box::new(Null),
                    Box::new(dk),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "(kind, dedupe_key) uniqueness across active rows",
        )?;
    }

    // Probe 3b: dedupe slot must keep blocking after `done`. The
    // brief's idempotency contract says a stable operation_id can't
    // be safely re-enqueued after it succeeded. A drifted index
    // narrowed to active states only would pass probe 3 but allow
    // the post-`done` replay we just hardened against. Insert a
    // `done` row with dedupe_key X, then try to enqueue a new
    // queued row with the same (kind, X). Expect: rejection.
    {
        let id_done = probe_id("dkd1");
        let id_q = probe_id("dkd2");
        let kind = probe_id("k");
        let dk = probe_id("dedupe");
        let done_insert = "INSERT INTO workflow_jobs \
                (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
                 base_backoff_ms, backoff_multiplier, max_backoff_ms, \
                 queue_key, dedupe_key, next_run_at, \
                 lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
                 enqueued_at, updated_at) \
             VALUES (?, ?, x'', 'done', 0, 0, 1, 0, 1, 0, \
                     NULL, ?, 0, NULL, NULL, NULL, NULL, NULL, 0, 0)";
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                done_insert,
                vec![
                    Box::new(id_done),
                    Box::new(kind.clone()),
                    Box::new(dk.clone()),
                ],
            )),
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id_q),
                    Box::new(kind),
                    Box::new(Null),
                    Box::new(dk),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "dedupe slot blocks replay after `done`",
        )?;
    }

    // Probe 3c: dedupe slot must release after terminal `failed`. A
    // drifted index broadened to include `failed` would pass probe 3
    // and 3b but make terminal-failed operations unreplayable. Insert
    // a `failed` row with dedupe_key X, then enqueue a new queued row
    // with the same (kind, X). Expect: accepted.
    {
        let id_failed = probe_id("dkf1");
        let id_q = probe_id("dkf2");
        let kind = probe_id("k");
        let dk = probe_id("dedupe");
        let failed_insert = "INSERT INTO workflow_jobs \
                (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
                 base_backoff_ms, backoff_multiplier, max_backoff_ms, \
                 queue_key, dedupe_key, next_run_at, \
                 lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
                 enqueued_at, updated_at) \
             VALUES (?, ?, x'', 'failed', 0, 0, 1, 0, 1, 0, \
                     NULL, ?, 0, NULL, NULL, NULL, NULL, NULL, 0, 0)";
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                failed_insert,
                vec![
                    Box::new(id_failed),
                    Box::new(kind.clone()),
                    Box::new(dk.clone()),
                ],
            )),
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id_q),
                    Box::new(kind),
                    Box::new(Null),
                    Box::new(dk),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::Accepted,
            "dedupe slot releases after terminal `failed`",
        )?;
    }

    // Probe 4: state-transition trigger — queued -> done is illegal.
    {
        let id = probe_id("st");
        let kind = probe_id("k");
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id.clone()),
                    Box::new(kind),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
            Box::new(ParamStmt::new(
                "UPDATE workflow_jobs SET state = 'done' WHERE job_id = ?",
                vec![Box::new(id)],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "queued -> done state transitions are blocked",
        )?;
    }

    // Probe 5: terminal-absorbing trigger — once 'failed', state
    // must not move back to 'queued' (or anywhere else). The fail
    // shape CHECK requires lease_owner / nonce / etc. all NULL.
    {
        let id = probe_id("ta");
        let kind = probe_id("k");
        let term_insert = "INSERT INTO workflow_jobs \
                (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
                 base_backoff_ms, backoff_multiplier, max_backoff_ms, \
                 queue_key, dedupe_key, next_run_at, \
                 lease_owner, lease_nonce, lease_started, lease_expires_at, last_error, \
                 enqueued_at, updated_at) \
             VALUES (?, ?, x'', 'failed', 0, 0, 1, 0, 1, 0, \
                     NULL, NULL, 0, NULL, NULL, NULL, NULL, NULL, 0, 0)";
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                term_insert,
                vec![Box::new(id.clone()), Box::new(kind)],
            )),
            Box::new(ParamStmt::new(
                "UPDATE workflow_jobs SET state = 'queued' WHERE job_id = ?",
                vec![Box::new(id)],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "terminal (failed) rows are absorbing",
        )?;
    }

    // Probe 5b: no-delete trigger — workflow_jobs rows must not be
    // hard-deleted. Direct DELETE bypasses the state machine and
    // (for a `done` row) frees its dedupe slot, letting an
    // already-completed side effect run again.
    {
        let id = probe_id("nd");
        let kind = probe_id("k");
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id.clone()),
                    Box::new(kind),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
            Box::new(ParamStmt::new(
                "DELETE FROM workflow_jobs WHERE job_id = ?",
                vec![Box::new(id)],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "workflow_jobs rows cannot be hard-deleted",
        )?;
    }

    // Probe 6: identity-immutable trigger — `kind` may not be
    // mutated post-insert.
    {
        let id = probe_id("ii");
        let kind = probe_id("k");
        let stmts: Vec<Box<dyn ProbeStmt>> = vec![
            Box::new(ParamStmt::new(
                PROBE_INSERT_QUEUED,
                vec![
                    Box::new(id.clone()),
                    Box::new(kind),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                    Box::new(Null),
                ],
            )),
            Box::new(ParamStmt::new(
                "UPDATE workflow_jobs SET kind = 'mutated' WHERE job_id = ?",
                vec![Box::new(id)],
            )),
        ];
        run_probes(
            conn,
            &stmts,
            ProbeExpect::ConstraintRejected,
            "identity columns (kind) are immutable",
        )?;
    }

    Ok(())
}

/// Owned parameter list for a probe statement so the runner can
/// rebuild the trait-object slice rusqlite needs.
struct ParamStmt {
    sql: &'static str,
    params: Vec<Box<dyn rusqlite::ToSql>>,
}

impl ParamStmt {
    fn new(sql: &'static str, params: Vec<Box<dyn rusqlite::ToSql>>) -> Self {
        Self { sql, params }
    }
}

trait ProbeStmt {
    fn run(&self, conn: &Connection) -> rusqlite::Result<usize>;
}

impl ProbeStmt for ParamStmt {
    fn run(&self, conn: &Connection) -> rusqlite::Result<usize> {
        let refs: Vec<&dyn rusqlite::ToSql> = self.params.iter().map(AsRef::as_ref).collect();
        conn.execute(self.sql, refs.as_slice())
    }
}

#[derive(Copy, Clone)]
enum ProbeExpect {
    /// The final statement is expected to be rejected by a constraint
    /// violation.
    ConstraintRejected,
    /// The final statement is expected to succeed (kept for
    /// future negative-shape probes; currently unused but the
    /// `run_probes` matrix handles both cases).
    #[allow(dead_code)]
    Accepted,
}

/// Run a sequence of probe statements inside a SAVEPOINT. Every
/// statement except the last must succeed cleanly (setup); the last
/// is matched against `expect`. Always rolls back so the DB is
/// unchanged regardless of outcome.
fn run_probes(
    conn: &Connection,
    stmts: &[Box<dyn ProbeStmt>],
    expect: ProbeExpect,
    invariant: &'static str,
) -> Result<(), SqliteJobStoreInitError> {
    conn.execute_batch("SAVEPOINT cairn_invariant_probe;")
        .map_err(|e| SqliteJobStoreInitError::Backend(e.to_string()))?;
    let last_idx = stmts.len().saturating_sub(1);
    let mut final_outcome = ProbeOutcome::Accepted;
    let mut setup_err: Option<String> = None;
    for (i, stmt) in stmts.iter().enumerate() {
        let outcome = classify(stmt.run(conn));
        if i == last_idx {
            final_outcome = outcome;
        } else {
            match outcome {
                ProbeOutcome::Accepted => {}
                ProbeOutcome::RejectedByConstraint => {
                    setup_err = Some(format!(
                        "setup statement {i} unexpectedly rejected by constraint"
                    ));
                    break;
                }
                ProbeOutcome::BackendError(msg) => {
                    setup_err = Some(format!("setup statement {i}: {msg}"));
                    break;
                }
            }
        }
    }
    conn.execute_batch(
        "ROLLBACK TO SAVEPOINT cairn_invariant_probe; \
         RELEASE SAVEPOINT cairn_invariant_probe;",
    )
    .map_err(|e| SqliteJobStoreInitError::Backend(e.to_string()))?;
    if let Some(msg) = setup_err {
        return Err(SqliteJobStoreInitError::Backend(format!(
            "probe `{invariant}`: {msg}"
        )));
    }
    match (expect, final_outcome) {
        (ProbeExpect::ConstraintRejected, ProbeOutcome::RejectedByConstraint)
        | (ProbeExpect::Accepted, ProbeOutcome::Accepted) => Ok(()),
        (ProbeExpect::ConstraintRejected, ProbeOutcome::Accepted)
        | (ProbeExpect::Accepted, ProbeOutcome::RejectedByConstraint) => Err(drift(invariant)),
        (_, ProbeOutcome::BackendError(msg)) => Err(SqliteJobStoreInitError::Backend(format!(
            "probe `{invariant}`: {msg}"
        ))),
    }
}

#[async_trait::async_trait]
impl JobStore for SqliteJobStore {
    async fn enqueue(&self, req: EnqueueRequest) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        // Move all owned data into the blocking task; rusqlite is sync.
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            insert_job(&mut guard, &req)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn lease(
        &self,
        owner: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<Option<LeasedJob>, JobStoreError> {
        if lease_duration_ms <= 0 {
            return Err(JobStoreError::InvalidLeaseDeadline {
                reason: format!("lease_duration_ms must be > 0 (got {lease_duration_ms})"),
            });
        }
        // Defend against overflow: a lease whose computed expiry
        // would wrap negative is meaningless and would persist as
        // already-expired.
        if now_ms.checked_add(lease_duration_ms).is_none() {
            return Err(JobStoreError::InvalidLeaseDeadline {
                reason: format!(
                    "now_ms + lease_duration_ms overflows i64 (now={now_ms}, dur={lease_duration_ms})"
                ),
            });
        }
        let conn = Arc::clone(&self.conn);
        let owner = owner.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            atomic_lease(&mut guard, &owner, now_ms, lease_duration_ms)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn heartbeat(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        now_ms: i64,
        new_expires_at_ms: i64,
    ) -> Result<(), JobStoreError> {
        if new_expires_at_ms <= now_ms {
            return Err(JobStoreError::InvalidLeaseDeadline {
                reason: format!(
                    "heartbeat must extend deadline into the future \
                     (now_ms={now_ms}, new_expires_at_ms={new_expires_at_ms})"
                ),
            });
        }
        // Heartbeats must not shrink an already-longer deadline:
        // moving a live deadline closer to now lets the reaper
        // reclaim a still-running worker. Strict monotonic extension.
        if new_expires_at_ms < lease.expires_at_ms {
            return Err(JobStoreError::InvalidLeaseDeadline {
                reason: format!(
                    "heartbeat must not shrink lease deadline \
                     (current={}, requested={new_expires_at_ms})",
                    lease.expires_at_ms
                ),
            });
        }
        let conn = Arc::clone(&self.conn);
        let job_id = job_id.clone();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            cas_heartbeat(&mut guard, &job_id, &lease, now_ms, new_expires_at_ms)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn complete(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        now_ms: i64,
    ) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        let job_id = job_id.clone();
        let lease = lease.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            cas_complete(&mut guard, &job_id, &lease, now_ms)
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn fail(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        disposition: FailDisposition,
        last_error: &str,
        now_ms: i64,
    ) -> Result<(), JobStoreError> {
        let conn = Arc::clone(&self.conn);
        let job_id = job_id.clone();
        let lease = lease.clone();
        let last_error = last_error.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut guard = match conn.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            cas_fail(
                &mut guard,
                &job_id,
                &lease,
                disposition,
                &last_error,
                now_ms,
            )
        })
        .await
        .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))?
    }

    async fn reap_expired(&self, now_ms: i64) -> Result<usize, JobStoreError> {
        // Drain in batches, releasing the store mutex between each so
        // concurrent heartbeats / completions / enqueues on this same
        // SqliteJobStore don't queue behind the entire backlog. Each
        // batch is its own spawn_blocking + lock acquisition: a
        // healthy worker can interleave between batches and avoid
        // missing its lease deadline even when the reaper has many
        // expired rows to process.
        let mut total = 0usize;
        loop {
            let conn = Arc::clone(&self.conn);
            let processed = tokio::task::spawn_blocking(move || {
                let mut guard = match conn.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                reap_batch(&mut guard, now_ms)
            })
            .await
            .map_err(|e| JobStoreError::Backend(format!("spawn_blocking join: {e}")))??;
            total = total.saturating_add(processed);
            if processed == 0 || i64::try_from(processed).unwrap_or(i64::MAX) < REAP_BATCH_SIZE {
                break;
            }
            // Yield to the runtime so other store operations can be
            // scheduled before we re-acquire the connection mutex.
            tokio::task::yield_now().await;
        }
        Ok(total)
    }
}

// ---- pure SQL helpers (sync, exercised both directly in tests and via the trait) ----

type LeaseRow = (String, String, Vec<u8>, i64, i64, i64, i64, i64);
// (attempts, max_attempts, base, mult, cap, lease_started)
type FailRow = (i64, i64, i64, i64, i64, i64);
// (job_id, attempts, delivery_count, max_attempts, base, mult, cap, lease_started)
type ReapRow = (String, i64, i64, i64, i64, i64, i64, i64);

fn insert_job(conn: &mut Connection, req: &EnqueueRequest) -> Result<(), JobStoreError> {
    // Use SQLite's wall clock for `enqueued_at`/`updated_at` so the
    // FIFO tie-breaker on `(next_run_at, enqueued_at)` reflects actual
    // insertion order — even when callers schedule future work with a
    // `not_before_ms` that intentionally differs from "now".
    let res = conn.execute(
        "INSERT INTO workflow_jobs \
            (job_id, kind, payload, state, attempts, delivery_count, max_attempts, \
             base_backoff_ms, backoff_multiplier, max_backoff_ms, \
             queue_key, dedupe_key, next_run_at, \
             lease_owner, lease_nonce, lease_started, lease_expires_at, \
             last_error, enqueued_at, updated_at) \
         VALUES (?, ?, ?, 'queued', 0, 0, ?, ?, ?, ?, ?, ?, ?, \
                 NULL, NULL, NULL, NULL, NULL, \
                 CAST(strftime('%s','now') AS INTEGER) * 1000, \
                 CAST(strftime('%s','now') AS INTEGER) * 1000)",
        params![
            req.job_id.as_str(),
            req.kind.as_str(),
            req.payload,
            i64::from(req.retry.max_attempts),
            i64::from(req.retry.base_backoff_ms),
            i64::from(req.retry.backoff_multiplier),
            i64::from(req.retry.max_backoff_ms),
            req.queue_key,
            req.dedupe_key,
            req.not_before_ms,
        ],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(translate_enqueue_err(&e, req)),
    }
}

fn retry_from_row(max_attempts: i64, base: i64, mult: i64, cap: i64) -> RetryPolicy {
    RetryPolicy {
        max_attempts: u32::try_from(max_attempts).unwrap_or(u32::MAX),
        base_backoff_ms: u32::try_from(base).unwrap_or(u32::MAX),
        backoff_multiplier: u32::try_from(mult).unwrap_or(u32::MAX),
        max_backoff_ms: u32::try_from(cap).unwrap_or(u32::MAX),
    }
}

fn translate_enqueue_err(e: &rusqlite::Error, req: &EnqueueRequest) -> JobStoreError {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    // SQLite UNIQUE-constraint errors name the columns ("workflow_jobs.kind,
    // workflow_jobs.dedupe_key"), not the index. Detect each conflict by
    // the column set.
    let unique_failed = lower.contains("unique constraint failed");
    if unique_failed
        && lower.contains("dedupe_key")
        && let Some(d) = &req.dedupe_key
    {
        return JobStoreError::DuplicateDedupeKey {
            kind: req.kind.clone(),
            dedupe_key: d.clone(),
        };
    }
    JobStoreError::Backend(msg)
}

fn atomic_lease(
    conn: &mut Connection,
    owner: &str,
    now_ms: i64,
    lease_duration_ms: i64,
) -> Result<Option<LeasedJob>, JobStoreError> {
    let new_expires = now_ms.saturating_add(lease_duration_ms);
    let nonce = ulid::Ulid::new().to_string();
    // BEGIN IMMEDIATE acquires a write lock up front so a
    // concurrent leaser/reaper on a separate connection retries via
    // `busy_timeout` instead of failing later with a stale-snapshot
    // error after the read.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    // Pick the oldest queued, ready row whose queue_key (if any) is
    // both (a) not currently leased and (b) head-of-line — no older
    // queued sibling for the same key. Together they guarantee
    // strict per-key FIFO even when retries push the head row's
    // next_run_at into the future: a younger sibling with an earlier
    // next_run_at cannot overtake. SQLite `strftime('%s','now')` is
    // second-resolution, so two enqueues in the same wall-clock
    // second can share `enqueued_at`; `rowid` is monotonic per
    // insert and serves as the deterministic FIFO tie-breaker.
    let candidate: Option<LeaseRow> = tx
        .query_row(
            "SELECT job_id, kind, payload, attempts, max_attempts, \
                    base_backoff_ms, backoff_multiplier, max_backoff_ms \
               FROM workflow_jobs AS j \
              WHERE state = 'queued' AND next_run_at <= ? \
                AND (queue_key IS NULL OR ( \
                    NOT EXISTS ( \
                        SELECT 1 FROM workflow_jobs AS l \
                         WHERE l.state = 'leased' \
                           AND l.queue_key = j.queue_key) \
                    AND NOT EXISTS ( \
                        SELECT 1 FROM workflow_jobs AS older \
                         WHERE older.state = 'queued' \
                           AND older.queue_key = j.queue_key \
                           AND (older.enqueued_at < j.enqueued_at \
                                OR (older.enqueued_at = j.enqueued_at \
                                    AND older.rowid < j.rowid))))) \
              ORDER BY next_run_at, enqueued_at, rowid \
              LIMIT 1",
            params![now_ms],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    let Some((job_id, kind, payload, attempts, max_attempts, base, mult, cap)) = candidate else {
        return Ok(None);
    };

    // CAS on (job_id, state='queued') so concurrent leasers can't both win.
    // `attempts` is NOT bumped here — it only consumes retry budget once
    // the worker proves it actually started by sending a heartbeat or
    // calling fail/complete. Otherwise an infrastructure crash between
    // lease and first execution would burn attempts without ever
    // running the workflow.
    // delivery_count increments on every lease — including those that
    // crash before the worker heartbeats. The reaper uses it to bound
    // never-started retries and to compute backoff.
    let updated = tx
        .execute(
            "UPDATE workflow_jobs \
                SET state = 'leased', lease_owner = ?, lease_nonce = ?, \
                    lease_started = 0, lease_expires_at = ?, \
                    delivery_count = delivery_count + 1, \
                    updated_at = ? \
              WHERE job_id = ? AND state = 'queued'",
            params![owner, nonce, new_expires, now_ms, job_id],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    if updated == 0 {
        // Lost the race; surface as "no work right now" — caller will retry.
        tx.rollback()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        return Ok(None);
    }

    tx.commit()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    // `attempts` reflects the worker's view of "how many times has this
    // run actually been tried" — equals the persisted column plus one,
    // because the first heartbeat (or fail/complete) the worker sends
    // will atomically bump the persisted counter to match.
    let new_attempts = u32::try_from(attempts.saturating_add(1)).unwrap_or(u32::MAX);
    let retry = retry_from_row(max_attempts, base, mult, cap);

    Ok(Some(LeasedJob {
        job_id: JobId::new(job_id),
        kind: JobKind::new(kind),
        payload,
        attempts: new_attempts,
        retry,
        lease: LeaseToken {
            owner: owner.to_owned(),
            nonce,
            expires_at_ms: new_expires,
        },
    }))
}

fn cas_heartbeat(
    conn: &mut Connection,
    job_id: &JobId,
    lease: &LeaseToken,
    now_ms: i64,
    new_expires_at_ms: i64,
) -> Result<(), JobStoreError> {
    // Expiry is enforced synchronously via `lease_expires_at > ?`; an
    // expired worker fails closed with LeaseLost without waiting for
    // the reaper to clear the row.
    //
    // First heartbeat after lease (lease_started=0) is the "I actually
    // started executing" signal — flip lease_started to 1 and bump
    // attempts. Subsequent heartbeats just extend the deadline.
    //
    // Monotonic-extension predicate `lease_expires_at <= ?` is part
    // of the same WHERE clause: a heartbeat must compare against the
    // *persisted* deadline, not the caller's stale token (the
    // contract reuses the same LeaseToken across heartbeats, so its
    // expires_at_ms drifts behind reality after the first renewal).
    // Splitting "lease lost" from "shrink rejected" requires a brief
    // pre-read inside an IMMEDIATE transaction.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let persisted: Option<i64> = tx
        .query_row(
            "SELECT lease_expires_at FROM workflow_jobs \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_nonce = ? \
                AND lease_expires_at > ?",
            params![job_id.as_str(), lease.owner, lease.nonce, now_ms],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let Some(persisted_expiry) = persisted else {
        let _ = tx.rollback();
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    };
    if new_expires_at_ms < persisted_expiry {
        let _ = tx.rollback();
        return Err(JobStoreError::InvalidLeaseDeadline {
            reason: format!(
                "heartbeat must not shrink lease deadline \
                 (persisted={persisted_expiry}, requested={new_expires_at_ms})"
            ),
        });
    }
    let updated = tx
        .execute(
            "UPDATE workflow_jobs \
                SET lease_expires_at = ?, \
                    attempts = CASE WHEN lease_started = 0 THEN attempts + 1 ELSE attempts END, \
                    lease_started = 1, \
                    updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_nonce = ? \
                AND lease_expires_at > ?",
            params![
                new_expires_at_ms,
                now_ms,
                job_id.as_str(),
                lease.owner,
                lease.nonce,
                now_ms,
            ],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    if updated == 0 {
        let _ = tx.rollback();
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    }
    tx.commit()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    Ok(())
}

fn cas_complete(
    conn: &mut Connection,
    job_id: &JobId,
    lease: &LeaseToken,
    now_ms: i64,
) -> Result<(), JobStoreError> {
    // `complete` implies the worker started executing — bump attempts
    // if no heartbeat already did so, then transition to terminal Done.
    let updated = conn
        .execute(
            "UPDATE workflow_jobs \
                SET state = 'done', lease_owner = NULL, lease_nonce = NULL, \
                    lease_started = NULL, lease_expires_at = NULL, \
                    attempts = CASE WHEN lease_started = 0 THEN attempts + 1 ELSE attempts END, \
                    updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_nonce = ? \
                AND lease_expires_at > ?",
            params![now_ms, job_id.as_str(), lease.owner, lease.nonce, now_ms],
        )
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    if updated == 0 {
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    }
    Ok(())
}

fn cas_fail(
    conn: &mut Connection,
    job_id: &JobId,
    lease: &LeaseToken,
    disposition: FailDisposition,
    last_error: &str,
    now_ms: i64,
) -> Result<(), JobStoreError> {
    // BEGIN IMMEDIATE acquires a write lock up front so a
    // concurrent leaser/reaper on a separate connection retries via
    // `busy_timeout` instead of failing later with a stale-snapshot
    // error after the read.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let row: Option<FailRow> = tx
        .query_row(
            "SELECT attempts, max_attempts, base_backoff_ms, backoff_multiplier, max_backoff_ms, \
                    lease_started \
               FROM workflow_jobs \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_nonce = ? \
                AND lease_expires_at > ?",
            params![job_id.as_str(), lease.owner, lease.nonce, now_ms],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;

    let Some((attempts, max_attempts, base, mult, cap, lease_started)) = row else {
        tx.rollback()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    };

    // Effective attempts after this fail: bump if heartbeat hasn't yet.
    let effective_attempts = if lease_started == 0 {
        attempts.saturating_add(1)
    } else {
        attempts
    };
    let exhausted = effective_attempts >= max_attempts;
    let terminal = matches!(disposition, FailDisposition::Permanent) || exhausted;

    let res = if terminal {
        tx.execute(
            "UPDATE workflow_jobs \
                SET state = 'failed', lease_owner = NULL, lease_nonce = NULL, \
                    lease_started = NULL, lease_expires_at = NULL, \
                    attempts = CASE WHEN lease_started = 0 THEN attempts + 1 ELSE attempts END, \
                    last_error = ?, updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_nonce = ? \
                AND lease_expires_at > ?",
            params![
                last_error,
                now_ms,
                job_id.as_str(),
                lease.owner,
                lease.nonce,
                now_ms,
            ],
        )
    } else {
        // Use the per-row retry policy, not RetryPolicy::DEFAULT, so a
        // caller's custom backoff actually applies after restart.
        let policy = retry_from_row(max_attempts, base, mult, cap);
        let attempt_for_delay = u32::try_from(effective_attempts).unwrap_or(u32::MAX);
        let delay = u64::from(policy.delay_for_attempt(attempt_for_delay));
        let next_run = now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
        tx.execute(
            "UPDATE workflow_jobs \
                SET state = 'queued', lease_owner = NULL, lease_nonce = NULL, \
                    lease_started = NULL, lease_expires_at = NULL, \
                    attempts = CASE WHEN lease_started = 0 THEN attempts + 1 ELSE attempts END, \
                    last_error = ?, next_run_at = ?, updated_at = ? \
              WHERE job_id = ? AND state = 'leased' \
                AND lease_owner = ? AND lease_nonce = ? \
                AND lease_expires_at > ?",
            params![
                last_error,
                next_run,
                now_ms,
                job_id.as_str(),
                lease.owner,
                lease.nonce,
                now_ms,
            ],
        )
    };
    let updated = res.map_err(|e| JobStoreError::Backend(e.to_string()))?;
    if updated == 0 {
        tx.rollback()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        return Err(JobStoreError::LeaseLost {
            job_id: job_id.clone(),
        });
    }
    tx.commit()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    Ok(())
}

/// Open a fully-migrated in-memory `SqliteJobStore` for tests.
/// Runs all `cairn-store-sqlite` migrations (which provision
/// `schema_migrations` + `workflow_jobs`) then wraps the connection in
/// a `SqliteJobStore`. Exposed `pub(crate)` so worker/reaper tests share
/// one bootstrap.
#[cfg(test)]
pub(crate) fn install_for_tests(conn: &rusqlite::Connection) {
    // `schema_migrations` is created by migration 0001, so we need the
    // full migration runner. Build a minimal schema_migrations table +
    // apply the workflow_jobs SQL directly so we don't depend on all
    // prior migrations in this unit test context.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (\
            migration_id INTEGER NOT NULL PRIMARY KEY, \
            name TEXT NOT NULL, \
            sql_hash TEXT NOT NULL DEFAULT '', \
            applied_at INTEGER NOT NULL \
         );",
    )
    .expect("create schema_migrations");
    conn.execute_batch(MIGRATION_0020_SQL).expect("apply 0020");
}

/// Maximum number of expired rows reaped under a single write
/// transaction. Caps how long the reaper can hold the database write
/// lock, so a backlog can't starve healthy workers' enqueue / lease /
/// heartbeat / complete writes (which only get a 5 s `busy_timeout`).
/// The reaper sweeps in successive batches until none remain.
const REAP_BATCH_SIZE: i64 = 64;

fn reap_batch(conn: &mut Connection, now_ms: i64) -> Result<usize, JobStoreError> {
    // BEGIN IMMEDIATE acquires a write lock up front so a
    // concurrent leaser/reaper on a separate connection retries via
    // `busy_timeout` instead of failing later with a stale-snapshot
    // error after the read. Bounded LIMIT keeps the lock hold time
    // short — see REAP_BATCH_SIZE.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let mut count = 0usize;
    {
        let mut stmt = tx
            .prepare(
                "SELECT job_id, attempts, delivery_count, max_attempts, \
                        base_backoff_ms, backoff_multiplier, max_backoff_ms, lease_started \
                   FROM workflow_jobs \
                  WHERE state = 'leased' AND lease_expires_at <= ? \
                  ORDER BY lease_expires_at \
                  LIMIT ?",
            )
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;
        let rows: Vec<ReapRow> = stmt
            .query_map(params![now_ms, REAP_BATCH_SIZE], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
                .map(|t: ReapRow| t)
            })
            .map_err(|e| JobStoreError::Backend(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| JobStoreError::Backend(e.to_string()))?;

        for (job_id, attempts, delivery_count, max_attempts, base, mult, cap, started) in rows {
            let policy = retry_from_row(max_attempts, base, mult, cap);
            // For started=1, heartbeat already bumped `attempts` — reap
            // doesn't bump again. For started=0, the lease counts as a
            // delivery but not an execution attempt — never-started
            // crashes do not consume `max_attempts` (the public retry
            // contract is in terms of executions; see RetryPolicy doc).
            //
            // Two terminal conditions:
            //   - executions exhausted (`attempts >= max_attempts`) —
            //     the worker ran but kept failing and ran out of budget.
            //   - poison guard (`delivery_count >= max_attempts * 5`) —
            //     a worker that keeps dying before first heartbeat
            //     would otherwise loop forever, blocking keyed queues.
            //     The 5× ceiling is implementation-defined and well
            //     above any plausible execution budget.
            let delivery_cap = max_attempts.saturating_mul(POISON_DELIVERY_MULTIPLIER);
            let exhausted = attempts >= max_attempts || delivery_count >= delivery_cap;
            if exhausted {
                tx.execute(
                    "UPDATE workflow_jobs \
                        SET state = 'failed', lease_owner = NULL, lease_nonce = NULL, \
                            lease_started = NULL, lease_expires_at = NULL, \
                            last_error = COALESCE(last_error, \
                                'lease expired with retry budget exhausted'), \
                            updated_at = ? \
                      WHERE job_id = ? AND state = 'leased'",
                    params![now_ms, job_id],
                )
                .map_err(|e| JobStoreError::Backend(e.to_string()))?;
            } else {
                // Apply per-row backoff. Use delivery_count for
                // never-started retries (so repeated pre-heartbeat
                // crashes back off too); use attempts for started
                // orphans (which already had attempts bumped on
                // heartbeat).
                let attempt_for_delay = if started == 0 {
                    u32::try_from(delivery_count).unwrap_or(u32::MAX)
                } else {
                    u32::try_from(attempts).unwrap_or(u32::MAX)
                };
                let delay = u64::from(policy.delay_for_attempt(attempt_for_delay));
                let next_run = now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
                tx.execute(
                    "UPDATE workflow_jobs \
                        SET state = 'queued', lease_owner = NULL, lease_nonce = NULL, \
                            lease_started = NULL, lease_expires_at = NULL, \
                            next_run_at = ?, updated_at = ? \
                      WHERE job_id = ? AND state = 'leased'",
                    params![next_run, now_ms, job_id],
                )
                .map_err(|e| JobStoreError::Backend(e.to_string()))?;
            }
            count += 1;
        }
    }
    tx.commit()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    Ok(count)
}
