// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::{RecordId, SourceRef};
use cairn_core::generated::verbs::lint::{Kind, Severity};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::sample_record;
use cairn_test_fixtures::store::FixtureStore;

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

fn modern_record(seed: u64, source_id: &str, bytes: &[u8]) -> cairn_core::domain::MemoryRecord {
    let mut record = sample_record(seed);
    let hash = sha256(bytes);
    hash.clone_into(&mut record.provenance.source_hash);
    record.provenance.source_refs = vec![SourceRef {
        id: source_id.to_owned(),
        hash,
    }];
    record
}

fn legacy_record(seed: u64, source_hash: &str) -> cairn_core::domain::MemoryRecord {
    let mut record = sample_record(seed);
    source_hash.clone_into(&mut record.provenance.source_hash);
    record.provenance.source_refs.clear();
    record
}

#[tokio::test]
async fn read_only_lint_flags_legacy_duplicate_without_mutating_source_file() {
    let store = FixtureStore::default();
    let source_id = "sources/chat/session-1.md";
    let source_bytes = b"stable source bytes for legacy duplicate";
    let modern = modern_record(10, source_id, source_bytes);
    let legacy = legacy_record(11, &modern.provenance.source_hash);
    store.upsert(&modern).await.expect("upsert modern");
    store.upsert(&legacy).await.expect("upsert legacy");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let vault = tempfile::tempdir().expect("tempdir");
    let abs = vault.path().join(source_id);
    std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
    std::fs::write(&abs, source_bytes).expect("write source");

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let duplicate = result
        .data
        .findings
        .iter()
        .find(|f| f.kind == Kind::SourceLinkLegacyDuplicate)
        .expect("legacy duplicate finding");
    assert_eq!(duplicate.severity, Severity::Error);
    assert_eq!(
        duplicate.target.as_ref().and_then(|t| t.record_id.as_ref()),
        Some(&cairn_core::generated::common::Ulid(
            modern.id.as_str().to_owned()
        ))
    );
    assert!(duplicate.message.contains(legacy.id.as_str()));

    let on_disk = std::fs::read(&abs).expect("read source back");
    assert_eq!(
        on_disk, source_bytes,
        "read-only lint must not mutate sources"
    );
}

#[tokio::test]
async fn read_only_lint_treats_directory_source_ref_as_dangling_io_case() {
    let store = FixtureStore::default();
    let source_id = "sources/chat/session-dir";
    let source_bytes = b"directory source bytes";
    let record = modern_record(12, source_id, source_bytes);
    store.upsert(&record).await.expect("upsert");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let vault = tempfile::tempdir().expect("tempdir");
    let abs = vault.path().join(source_id);
    std::fs::create_dir_all(&abs).expect("mkdir");

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let dangling = result
        .data
        .findings
        .iter()
        .find(|f| f.kind == Kind::SourceLinkDangling)
        .expect("dangling finding");
    assert_eq!(dangling.severity, Severity::Error);
    assert_eq!(
        dangling.target.as_ref().and_then(|t| t.record_id.as_ref()),
        Some(&cairn_core::generated::common::Ulid(
            record.id.as_str().to_owned()
        ))
    );
    assert!(dangling.message.contains(source_id));
}

#[tokio::test]
async fn read_only_lint_reports_missing_source_refs_as_error_e2e() {
    let store = FixtureStore::default();
    let record = sample_record(13);
    let record_id = RecordId::parse(record.id.as_str()).expect("record id");
    store.upsert(&record).await.expect("upsert");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let vault = tempfile::tempdir().expect("tempdir");

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let missing = result
        .data
        .findings
        .iter()
        .find(|f| f.kind == Kind::SourceLinkMissing)
        .expect("missing source_ref finding");
    assert_eq!(missing.severity, Severity::Error);
    assert_eq!(
        missing.target.as_ref().and_then(|t| t.record_id.as_ref()),
        Some(&cairn_core::generated::common::Ulid(
            record_id.as_str().to_owned()
        ))
    );
}
