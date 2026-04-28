//! Migration ledger. Identical for every build flavor — no feature gates
//! change which numbered migrations exist. Checksums are computed at
//! build time by `build.rs` and embedded via `include!`.

pub mod runner;

use std::sync::LazyLock;

/// A single database migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// 1-based integer id matching the ledger `id` column.
    pub id: u32,
    /// Filename of the SQL file (e.g. `0002_records.sql`).
    pub name: &'static str,
    /// Full SQL text embedded at compile time.
    pub sql: &'static str,
    /// SHA-256 hex checksum of the raw SQL bytes, computed by `build.rs`.
    pub checksum: &'static str,
}

/// `(filename, sha256_hex)` pairs produced by `build.rs`.
static NAMED_CHECKSUMS: LazyLock<&'static [(&'static str, &'static str)]> =
    LazyLock::new(|| include!(concat!(env!("OUT_DIR"), "/migration_checksums.rs")));

fn checksum_for(name: &str) -> &'static str {
    // SAFETY-INVARIANT: build.rs enumerates migrations/ and emits one checksum
    // per .sql file. A missing entry means the build script did not run or the
    // migration name is misspelled — both are build-time bugs.
    NAMED_CHECKSUMS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or_else(
            || panic!("no checksum found for migration '{name}': check build.rs output"),
            |(_, c)| *c,
        )
}

/// Ordered migration list. Applied sequentially by `runner::apply_pending`.
pub static MIGRATIONS: LazyLock<Vec<Migration>> = LazyLock::new(|| {
    vec![
        Migration {
            id: 1,
            name: "0001_init_pragmas.sql",
            sql: include_str!("../../migrations/0001_init_pragmas.sql"),
            checksum: checksum_for("0001_init_pragmas.sql"),
        },
        Migration {
            id: 2,
            name: "0002_records.sql",
            sql: include_str!("../../migrations/0002_records.sql"),
            checksum: checksum_for("0002_records.sql"),
        },
        Migration {
            id: 3,
            name: "0003_edges.sql",
            sql: include_str!("../../migrations/0003_edges.sql"),
            checksum: checksum_for("0003_edges.sql"),
        },
        Migration {
            id: 4,
            name: "0004_fts5.sql",
            sql: include_str!("../../migrations/0004_fts5.sql"),
            checksum: checksum_for("0004_fts5.sql"),
        },
        Migration {
            id: 5,
            name: "0005_wal_state.sql",
            sql: include_str!("../../migrations/0005_wal_state.sql"),
            checksum: checksum_for("0005_wal_state.sql"),
        },
        Migration {
            id: 6,
            name: "0006_replay_consent.sql",
            sql: include_str!("../../migrations/0006_replay_consent.sql"),
            checksum: checksum_for("0006_replay_consent.sql"),
        },
        Migration {
            id: 7,
            name: "0007_locks_jobs.sql",
            sql: include_str!("../../migrations/0007_locks_jobs.sql"),
            checksum: checksum_for("0007_locks_jobs.sql"),
        },
        Migration {
            id: 8,
            name: "0008_meta.sql",
            sql: include_str!("../../migrations/0008_meta.sql"),
            checksum: checksum_for("0008_meta.sql"),
        },
        Migration {
            id: 9,
            name: "0009_add_record_json.sql",
            sql: include_str!("../../migrations/0009_add_record_json.sql"),
            checksum: checksum_for("0009_add_record_json.sql"),
        },
        Migration {
            id: 10,
            name: "0010_purge_scope_snapshot.sql",
            sql: include_str!("../../migrations/0010_purge_scope_snapshot.sql"),
            checksum: checksum_for("0010_purge_scope_snapshot.sql"),
        },
    ]
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_dense_and_ordered() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                usize::try_from(m.id).expect("id fits usize"),
                i + 1,
                "migration {i} has wrong id: {}",
                m.name
            );
        }
    }

    #[test]
    fn checksums_are_64_hex() {
        for m in MIGRATIONS.iter() {
            assert_eq!(
                m.checksum.len(),
                64,
                "checksum for {} is not 64 chars",
                m.name
            );
            assert!(
                m.checksum.chars().all(|c| c.is_ascii_hexdigit()),
                "checksum for {} contains non-hex chars",
                m.name
            );
        }
    }
}
