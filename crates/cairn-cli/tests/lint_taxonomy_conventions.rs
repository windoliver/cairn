//! End-to-end coverage for taxonomy-convention lint rules.

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::record::RecordId;
use cairn_core::domain::{MemoryClass, MemoryKind, TargetId};
use cairn_core::generated::verbs::lint::{Kind, Severity};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::store::{FixtureStore, sample_record};

fn record(
    record_id: &str,
    kind: MemoryKind,
    class: MemoryClass,
) -> cairn_core::domain::MemoryRecord {
    let mut record = sample_record();
    record.id = RecordId::parse(record_id).expect("valid record id");
    record.target_id = TargetId::parse(record_id).expect("valid target id");
    record.kind = kind;
    record.class = class;
    record
}

#[tokio::test]
async fn read_only_lint_reports_taxonomy_conventions_from_store_records() {
    let store = FixtureStore::default();

    let mut profile_one = record(
        "01ARZ3NDEKTSV4RRFFQ69G5F00",
        MemoryKind::User,
        MemoryClass::Semantic,
    );
    profile_one.extra_frontmatter.insert(
        "well_known_id".to_owned(),
        serde_json::json!("profile:hmn:alice"),
    );
    let mut profile_two = record(
        "01ARZ3NDEKTSV4RRFFQ69G5F01",
        MemoryKind::User,
        MemoryClass::Semantic,
    );
    profile_two.extra_frontmatter.insert(
        "well_known_id".to_owned(),
        serde_json::json!("profile:hmn:alice"),
    );
    let wrong_class = record(
        "01ARZ3NDEKTSV4RRFFQ69G5F02",
        MemoryKind::Fact,
        MemoryClass::Episodic,
    );
    let sourced_belief = record(
        "01ARZ3NDEKTSV4RRFFQ69G5F03",
        MemoryKind::Belief,
        MemoryClass::Semantic,
    );

    for record in [&profile_one, &profile_two, &wrong_class, &sourced_belief] {
        store.upsert(record).await.expect("upsert record");
    }

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let vault = tempfile::tempdir().expect("tempdir");

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let taxonomy_findings: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.kind,
                Kind::MisclassifiedProfile | Kind::OrphanInsight | Kind::WrongClassForKind
            )
        })
        .collect();

    assert_eq!(taxonomy_findings.len(), 2, "{taxonomy_findings:#?}");
    assert!(
        taxonomy_findings
            .iter()
            .all(|f| f.severity == Severity::Warning)
    );
    assert!(
        taxonomy_findings
            .iter()
            .all(|f| { f.message.contains("docs/design/taxonomy-conventions.md") })
    );
    assert_eq!(
        taxonomy_findings
            .iter()
            .filter(|f| f.kind == Kind::MisclassifiedProfile)
            .count(),
        1
    );
    assert_eq!(
        taxonomy_findings
            .iter()
            .filter(|f| f.kind == Kind::WrongClassForKind)
            .count(),
        1
    );
    assert!(
        taxonomy_findings
            .iter()
            .all(|f| f.kind != Kind::OrphanInsight),
        "sourced belief should not be treated as orphan insight"
    );
    assert_eq!(
        result.data.summary.by_kind["misclassified_profile"],
        serde_json::json!(1)
    );
    assert_eq!(
        result.data.summary.by_kind["wrong_class_for_kind"],
        serde_json::json!(1)
    );
}
