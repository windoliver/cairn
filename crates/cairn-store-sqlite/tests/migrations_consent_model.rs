//! 0022 `records.consent_model` — Phase-A column with backward-compat default.
//!
//! Brief §14, Issue #253.

use cairn_store_sqlite::open_in_memory_sync;

#[test]
fn records_table_has_consent_model_column_with_legacy_default() {
    let conn = open_in_memory_sync().expect("open in-memory store");

    // Column exists, NOT NULL, default 'legacy_event'.
    let (name, notnull, dflt): (String, i64, Option<String>) = conn
        .query_row(
            "SELECT name, \"notnull\", dflt_value \
               FROM pragma_table_info('records') \
              WHERE name = 'consent_model'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("consent_model column exists");

    assert_eq!(name, "consent_model");
    assert_eq!(notnull, 1, "consent_model must be NOT NULL");
    assert_eq!(
        dflt.as_deref(),
        Some("'legacy_event'"),
        "default must be 'legacy_event' for Phase-A backward compat"
    );

    // CHECK constraint enforces the closed enum.
    let bad = conn.execute(
        "INSERT INTO records (record_id, target_id, version, path, kind, class, \
         visibility, scope, actor_chain, body, body_hash, created_at, updated_at, \
         active, tombstoned, is_static, consent_model) \
         VALUES ('r','t',1,'p','user','user','private','private','[]','b','h:0',0,0,0,0,0,'banana')",
        [],
    );
    assert!(
        bad.is_err(),
        "CHECK constraint must reject unknown values, got {bad:?}",
    );
}

#[test]
fn migration_head_is_at_least_22() {
    let conn = open_in_memory_sync().expect("open in-memory store");
    let head: i64 = conn
        .query_row("SELECT MAX(migration_id) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("query head");
    assert!(head >= 22, "0022 must be applied; head={head}");
}
